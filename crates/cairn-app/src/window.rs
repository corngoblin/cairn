//! The window: a list of installations, one button each, and an honest progress bar.
//!
//! The shape follows from what the tool is for. There is exactly one primary action
//! per installation — extract, or update once something is extracted — because the
//! difference between the two is a consequence of state, not a choice the user
//! should have to make. Everything rarer hangs off the row's menu: destinations,
//! verify, undo, forget.
//!
//! All the real work happens in [`crate::worker`]; this file only draws state and
//! asks for changes.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use cairn_core::engine::{self, RunKind, UndoSummary};
use cairn_core::manifest::{self, Manifest};
use cairn_core::paths::LANGUAGES;
use cairn_core::settings::{Config, Destination, Installation};
use cairn_core::util;
use cairn_core::Phase;

use crate::worker::{self, Msg, Running};

pub struct CairnWindow {
    window: adw::ApplicationWindow,
    config_path: PathBuf,
    config: RefCell<Config>,

    /// The two content pages: the empty state, and the list of installations.
    stack: gtk::Stack,
    list: gtk::ListBox,

    /// Shown while a run is in flight.
    activity: gtk::Revealer,
    activity_label: gtk::Label,
    progress: gtk::ProgressBar,
    cancel_button: gtk::Button,

    toast_overlay: adw::ToastOverlay,
    busy: Cell<bool>,
    running: RefCell<Option<Running>>,
}

impl CairnWindow {
    pub fn build(app: &adw::Application) -> Rc<Self> {
        let config_path = Config::default_path();
        let config = Config::load(&config_path);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(cairn_core::APP_NAME)
            .default_width(860)
            .default_height(620)
            .build();

        // --- header -------------------------------------------------------
        let header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new(
            cairn_core::APP_NAME,
            "Extract D2R's data files so it loads from disk",
        );
        header.set_title_widget(Some(&title));

        let scan_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Look for installations on this machine")
            .build();
        scan_button.set_action_name(Some("win.scan"));
        header.pack_start(&scan_button);

        let add_button = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Add a game folder")
            .build();
        add_button.set_action_name(Some("win.add-folder"));
        header.pack_start(&add_button);

        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Menu")
            .build();
        let menu = gio::Menu::new();
        menu.append(Some("Settings"), Some("win.settings"));
        menu.append(Some("About Cairn"), Some("win.about"));
        menu_button.set_menu_model(Some(&menu));
        header.pack_end(&menu_button);

        // --- the list -----------------------------------------------------
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");
        // Rows keep their natural height and stack from the top: a list that stretches
        // its only row to fill the window reads as a missing second row.
        list.set_valign(gtk::Align::Start);
        list.set_margin_top(18);
        list.set_margin_bottom(18);
        list.set_margin_start(18);
        list.set_margin_end(18);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        let clamp = adw::Clamp::builder()
            .maximum_size(760)
            .tightening_threshold(600)
            .child(&scrolled)
            .build();

        // --- the empty state ---------------------------------------------
        let empty_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        empty_buttons.set_halign(gtk::Align::Center);
        let scan = gtk::Button::with_label("Look for installations");
        scan.add_css_class("suggested-action");
        scan.set_action_name(Some("win.scan"));
        let add = gtk::Button::with_label("Choose a folder…");
        add.set_action_name(Some("win.add-folder"));
        empty_buttons.append(&scan);
        empty_buttons.append(&add);

        let empty = adw::StatusPage::builder()
            .icon_name("drive-harddisk-symbolic")
            .title("No installations yet")
            .description(
                "Cairn unpacks Diablo II: Resurrected's archives into plain files, which is \
                 where the shorter loading times come from. Point it at the folder that holds \
                 the game — Battle.net, Steam and a Proton prefix all work.",
            )
            .child(&empty_buttons)
            .build();

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&clamp, Some("list"));

        // --- activity bar -------------------------------------------------
        let activity_label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        activity_label.add_css_class("caption");
        let progress = gtk::ProgressBar::new();
        progress.set_show_text(false);
        progress.set_hexpand(true);
        let cancel_button = gtk::Button::with_label("Cancel");
        cancel_button.set_valign(gtk::Align::Center);

        let activity_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        activity_row.append(&progress);
        activity_row.append(&cancel_button);

        let activity_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        activity_box.set_margin_top(10);
        activity_box.set_margin_bottom(10);
        activity_box.set_margin_start(18);
        activity_box.set_margin_end(18);
        activity_box.append(&activity_label);
        activity_box.append(&activity_row);

        let activity = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .child(&activity_box)
            .reveal_child(false)
            .build();

        // --- assemble ------------------------------------------------------
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_bottom_bar(&activity);
        toolbar.set_content(Some(&stack));

        let toast_overlay = adw::ToastOverlay::new();
        toast_overlay.set_child(Some(&toolbar));
        window.set_content(Some(&toast_overlay));

        let this = Rc::new(Self {
            window,
            config_path,
            config: RefCell::new(config),
            stack,
            list,
            activity,
            activity_label,
            progress,
            cancel_button,
            toast_overlay,
            busy: Cell::new(false),
            running: RefCell::new(None),
        });

        this.wire();
        this.refresh();

        // One line at startup, on stderr, so a packaged run can be diagnosed without
        // pixels: `flatpak run …` collects it, and it says whether the window really
        // got built and what it thinks it has to show.
        let (count, active) = {
            let config = this.config.borrow();
            (
                config.installations.len(),
                config
                    .installations
                    .iter()
                    .map(|i| i.active_destinations().len())
                    .sum::<usize>(),
            )
        };
        eprintln!(
            "cairn {} ready: {count} installation(s), {active} active destination(s), config {}",
            cairn_core::VERSION,
            this.config_path.display()
        );

        this
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// Wires the window-level actions every row and the header refer to by name.
    fn wire(self: &Rc<Self>) {
        self.action("scan", |this| this.scan());
        self.action("add-folder", |this| this.add_folder());
        self.action("settings", |this| this.open_settings());
        self.action("about", |this| this.open_about());
        self.action_param("extract", |this, index| this.start_extract(index));
        self.action_param("destinations", |this, index| this.open_destinations(index));
        self.action_param("verify", |this, index| this.verify(index));
        self.action_param("undo", |this, index| this.undo(index));
        self.action_param("forget", |this, index| this.forget(index));

        let this = Rc::clone(self);
        self.cancel_button.connect_clicked(move |_| {
            if let Some(running) = this.running.borrow().as_ref() {
                running.cancel();
                this.activity_label.set_text("Cancelling…");
            }
        });
    }

    fn action(self: &Rc<Self>, name: &str, run: impl Fn(&Rc<Self>) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        let this = Rc::clone(self);
        action.connect_activate(move |_, _| run(&this));
        self.window.add_action(&action);
    }

    fn action_param(self: &Rc<Self>, name: &str, run: impl Fn(&Rc<Self>, usize) + 'static) {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::UINT32));
        let this = Rc::clone(self);
        action.connect_activate(move |_, param| {
            if let Some(index) = param.and_then(|p| p.get::<u32>()) {
                run(&this, index as usize);
            }
        });
        self.window.add_action(&action);
    }

    // -----------------------------------------------------------------------
    // Drawing state
    // -----------------------------------------------------------------------

    /// Redraws the list from the config, and picks which page is showing.
    fn refresh(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }

        let config = self.config.borrow();
        for (index, installation) in config.installations.iter().enumerate() {
            self.list
                .append(&self.installation_row(installation, index));
        }

        let has_any = !config.installations.is_empty();
        self.stack
            .set_visible_child_name(if has_any { "list" } else { "empty" });
    }

    fn installation_row(
        self: &Rc<Self>,
        installation: &Installation,
        index: usize,
    ) -> adw::ActionRow {
        let row = adw::ActionRow::new();
        row.set_title(&glib::markup_escape_text(&installation.display_name()));

        let destinations = installation.active_destinations();
        let primary = destinations.first().map(|d| d.path.clone());

        // The button says what it will do, because that is the only difference the
        // user needs to know about between a first run and a later one.
        let (label, is_update) = match &primary {
            Some(path) => match Manifest::load(path) {
                Ok(Some(m)) if m.complete => ("Update", true),
                Ok(Some(_)) => ("Resume", true),
                _ => ("Extract", false),
            },
            None => ("Extract", false),
        };

        let mut subtitle = match &primary {
            Some(path) => manifest::describe(path),
            None => "no destination enabled".to_string(),
        };
        if destinations.len() > 1 {
            subtitle = format!("{subtitle}  ·  {} destinations", destinations.len());
        }
        if !installation.path.is_dir() {
            subtitle = "game folder is missing".to_string();
        } else if !cairn_core::detect::is_installation(&installation.path) {
            subtitle = format!("{subtitle}  ·  no game archives found here");
        }
        row.set_subtitle(&glib::markup_escape_text(&subtitle));

        let button = gtk::Button::with_label(label);
        button.set_valign(gtk::Align::Center);
        if !is_update {
            button.add_css_class("suggested-action");
        }
        button.set_action_name(Some("win.extract"));
        button.set_action_target_value(Some(&(index as u32).to_variant()));
        button.set_sensitive(!self.busy.get());
        row.add_suffix(&button);

        let menu_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("More")
            .build();
        menu_button.add_css_class("flat");

        let menu = gio::Menu::new();
        // The target has to be a real uint32 variant: a menu item's action string
        // ("win.destinations(0)") is parsed as *int32*, which never matches an action
        // declared with a `u` parameter — the item then renders greyed out and looks
        // like a missing feature rather than a wrong type.
        menu.append_item(&menu_item("Destinations…", "win.destinations", index));
        menu.append_item(&menu_item("Verify files", "win.verify", index));
        menu.append_item(&menu_item("Undo extraction", "win.undo", index));
        menu.append_item(&menu_item("Forget this folder", "win.forget", index));
        menu_button.set_menu_model(Some(&menu));
        row.add_suffix(&menu_button);

        row
    }

    // -----------------------------------------------------------------------
    // Config edits
    // -----------------------------------------------------------------------

    fn save(self: &Rc<Self>) {
        if let Err(e) = self.config.borrow().save(&self.config_path) {
            self.toast(&format!("Could not save settings: {e}"));
        }
    }

    fn scan(self: &Rc<Self>) {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let Some(home) = home else {
            self.toast("HOME is not set");
            return;
        };
        let added = self.config.borrow_mut().discover(&home);
        self.save();
        self.refresh();
        let message = if added == 0 {
            "No new installations found. Use “Choose a folder…” if yours is somewhere unusual."
                .to_string()
        } else {
            format!(
                "Added {added} installation{}. Point it at the game with -direct -txt after \
                 extracting.",
                if added == 1 { "" } else { "s" }
            )
        };
        self.toast(&message);
    }

    fn add_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Choose the Diablo II: Resurrected folder")
            .modal(true)
            .build();

        let this = Rc::clone(self);
        let window = self.window.clone();
        dialog.select_folder(Some(&window), None::<&gio::Cancellable>, move |result| {
            let Ok(folder) = result else { return };
            let Some(path) = folder.path() else { return };
            if !cairn_core::detect::is_installation(&path) {
                this.toast(
                    "That folder has no game archives in it. Pick the folder containing the \
                         Data folder.",
                );
                return;
            }
            let added = this.config.borrow_mut().add_installation(path);
            match added {
                Ok(_) => {
                    this.save();
                    this.refresh();
                    this.toast("Installation added. Press Extract when you are ready.");
                }
                Err(e) => this.toast(&e),
            }
        });
    }

    fn forget(self: &Rc<Self>, index: usize) {
        let path = {
            let config = self.config.borrow();
            match config.installations.get(index) {
                Some(i) => i.path.clone(),
                None => return,
            }
        };
        self.config.borrow_mut().remove_installation(&path);
        self.save();
        self.refresh();
        self.toast("Removed from the list. Files already extracted were left alone.");
    }

    // -----------------------------------------------------------------------
    // Destinations
    // -----------------------------------------------------------------------

    fn open_destinations(self: &Rc<Self>, index: usize) {
        if self.config.borrow().installations.get(index).is_none() {
            return;
        }

        let dialog = adw::Dialog::builder()
            .title("Destinations")
            .content_width(560)
            .content_height(460)
            .build();

        let header = adw::HeaderBar::new();
        let add = gtk::Button::with_label("Add folder…");
        header.pack_start(&add);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");
        list.set_valign(gtk::Align::Start);
        list.set_margin_top(18);
        list.set_margin_bottom(18);
        list.set_margin_start(18);
        list.set_margin_end(18);

        let note = gtk::Label::builder()
            .label(
                "Every Extract and Update writes to all of these. Only the game folder is read \
                 by -direct mode; the others are for a mods folder or a copy you patch by hand.",
            )
            .wrap(true)
            .xalign(0.0)
            .build();
        note.add_css_class("dim-label");
        note.add_css_class("caption");
        note.set_margin_top(18);
        note.set_margin_bottom(6);
        note.set_margin_start(18);
        note.set_margin_end(18);

        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        group.set_title("Where files are written");
        group.add(&list);
        page.add(&group);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&note);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&page)
            .build();
        content.append(&scrolled);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        dialog.set_child(Some(&toolbar));

        // The rows are filled by a free function rather than a closure that calls
        // itself: refreshing after a switch flip is a call, not a recursion.
        fill_destinations(self, index, &list);

        let this_add = Rc::clone(self);
        let list_for_add = list.clone();
        // A file chooser needs a GtkWindow parent, which an AdwDialog is not; the
        // window behind the dialog is the right parent and leaves the dialog up.
        let parent_for_add = self.window.clone();
        add.connect_clicked(move |_| {
            let chooser = gtk::FileDialog::builder()
                .title("Choose a folder to extract into")
                .modal(true)
                .build();
            let this = Rc::clone(&this_add);
            let list = list_for_add.clone();
            chooser.select_folder(
                Some(&parent_for_add),
                None::<&gio::Cancellable>,
                move |result| {
                    let Ok(folder) = result else { return };
                    let Some(path) = folder.path() else { return };
                    let outcome = {
                        let mut config = this.config.borrow_mut();
                        match config.installations.get_mut(index) {
                            Some(installation) => installation.add_destination(path),
                            None => return,
                        }
                    };
                    match outcome {
                        Ok(()) => {
                            this.save();
                            fill_destinations(&this, index, &list);
                        }
                        Err(message) => this.toast(&message),
                    }
                },
            );
        });

        dialog.present(Some(&self.window));
    }

    // -----------------------------------------------------------------------
    // Runs
    // -----------------------------------------------------------------------

    fn start_extract(self: &Rc<Self>, index: usize) {
        if self.busy.get() {
            self.toast("An extraction is already running.");
            return;
        }
        let Some(installation) = self.config.borrow().installations.get(index).cloned() else {
            return;
        };
        if !installation.path.is_dir() {
            self.toast("That folder no longer exists.");
            return;
        }
        if installation.active_destinations().is_empty() {
            self.toast("Every destination is switched off, so there is nowhere to write.");
            return;
        }

        let (language, verify_contents) = {
            let config = self.config.borrow();
            (config.language.clone(), config.verify_contents)
        };

        let running = worker::spawn_extract(installation, language, verify_contents);
        self.begin_run("Starting…", running);
    }

    fn undo(self: &Rc<Self>, index: usize) {
        if self.busy.get() {
            self.toast("Wait for the current operation to finish.");
            return;
        }
        let Some(destination) = self
            .config
            .borrow()
            .installations
            .get(index)
            .and_then(|i| i.active_destinations().first().map(|d| d.path.clone()))
        else {
            return;
        };

        // Undo deletes tens of gigabytes, so it asks first — with the number of
        // files that are about to go, not a generic warning.
        let recorded = Manifest::load(&destination)
            .ok()
            .flatten()
            .map(|m| (m.entry_count(), m.total_bytes()));

        let body = match recorded {
            Some((files, bytes)) => format!(
                "This removes the {files} files Cairn wrote to\n{}\n({}). Files you put there \
                 yourself are left alone, and the archives are not touched either way.",
                destination.display(),
                util::format_bytes(bytes)
            ),
            None => format!(
                "Nothing is recorded in {}\nso there is nothing to undo.",
                destination.display()
            ),
        };

        if recorded.is_none() {
            self.toast("Nothing to undo there.");
            return;
        }

        let alert = adw::AlertDialog::new(Some("Undo extraction?"), Some(&body));
        alert.add_response("cancel", "Cancel");
        alert.add_response("undo", "Remove extracted files");
        alert.set_response_appearance("undo", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");

        let this = Rc::clone(self);
        alert.connect_response(None, move |_, response| {
            if response == "undo" {
                let running = worker::spawn_undo(destination.clone());
                this.begin_run("Removing extracted files…", running);
            }
        });
        alert.present(Some(&self.window));
    }

    fn verify(self: &Rc<Self>, index: usize) {
        if self.busy.get() {
            self.toast("Wait for the current operation to finish.");
            return;
        }
        let Some(destination) = self
            .config
            .borrow()
            .installations
            .get(index)
            .and_then(|i| i.active_destinations().first().map(|d| d.path.clone()))
        else {
            return;
        };
        let running = worker::spawn_verify(destination);
        self.begin_run("Check-summing extracted files…", running);
    }

    /// Puts the window into its busy state and starts draining the worker's channel.
    fn begin_run(self: &Rc<Self>, label: &str, running: Running) {
        self.busy.set(true);
        self.activity_label.set_text(label);
        self.progress.set_fraction(0.0);
        self.activity.set_reveal_child(true);
        self.running.replace(Some(running));
        self.refresh();

        let this = Rc::clone(self);
        glib::timeout_add_local(Duration::from_millis(100), move || {
            this.drain();
            if this.busy.get() {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
    }

    /// Reads everything the worker has said since the last tick.
    fn drain(self: &Rc<Self>) {
        let Some(running) = self.running.borrow().as_ref().map(|r| r.cancel.clone()) else {
            return;
        };
        let _ = running;

        // Messages are pulled out before anything is drawn, so no borrow is held
        // while a callback runs.
        let mut messages = Vec::new();
        {
            let guard = self.running.borrow();
            if let Some(running) = guard.as_ref() {
                while let Ok(message) = running.rx.try_recv() {
                    messages.push(message);
                }
            }
        }

        for message in messages {
            match message {
                Msg::Progress(progress) => self.show_progress(&progress),
                Msg::Log(line) => {
                    self.activity_label.set_text(&line);
                }
                Msg::Extracted(results) => self.finish_extract(results),
                Msg::Undone(result) => self.finish_undo(result),
                Msg::Verified(result) => self.finish_verify(result),
                Msg::Failed(message) => {
                    self.end_run();
                    self.toast(&message);
                }
            }
        }
    }

    fn show_progress(&self, progress: &cairn_core::Progress) {
        let label = if progress.current.is_empty() {
            progress.phase.label().to_string()
        } else {
            format!("{}: {}", progress.phase.label(), progress.current)
        };
        self.activity_label.set_text(&label);

        match (progress.phase, progress.fraction()) {
            (Phase::Enumerating, _) | (Phase::Planning, _) => {
                // Indeterminate: pulse, and say what it is doing instead of measuring.
                self.progress.pulse();
            }
            (_, Some(fraction)) => self.progress.set_fraction(fraction),
            _ => self.progress.pulse(),
        }
    }

    fn finish_extract(
        self: &Rc<Self>,
        results: Vec<Result<(PathBuf, engine::RunSummary), (PathBuf, engine::RunError)>>,
    ) {
        self.end_run();
        let mut lines: Vec<String> = Vec::new();
        let mut cancelled = false;

        for result in results {
            match result {
                Ok((path, summary)) => match summary.kind {
                    RunKind::UpToDate => lines.push(format!(
                        "{}: already up to date ({} files)",
                        path.display(),
                        summary.unchanged
                    )),
                    kind => lines.push(format!(
                        "{}: {} — {} files, {} in {}",
                        path.display(),
                        kind.label(),
                        summary.written,
                        util::format_bytes(summary.bytes_written),
                        util::format_duration(summary.elapsed_secs)
                    )),
                },
                Err((path, engine::RunError::Cancelled)) => {
                    cancelled = true;
                    lines.push(format!("{}: cancelled", path.display()));
                }
                Err((path, e)) => lines.push(format!("{}: failed — {e}", path.display())),
            }
        }

        let toast = if cancelled {
            "Cancelled. Nothing was left half-written; press Extract to carry on.".to_string()
        } else if lines.len() == 1 {
            lines.remove(0)
        } else {
            format!(
                "{} destinations written. See the list for details.",
                lines.len()
            )
        };
        self.toast(&toast);
    }

    fn finish_undo(self: &Rc<Self>, result: Result<UndoSummary, engine::RunError>) {
        self.end_run();
        match result {
            Ok(summary) if summary.is_noop() => {
                self.toast("Nothing was recorded, so nothing was removed.")
            }
            Ok(summary) => self.toast(&format!(
                "Removed {} files. Take -direct -txt out of the launch options.",
                summary.removed
            )),
            Err(e) => self.toast(&format!("Undo failed: {e}")),
        }
    }

    fn finish_verify(self: &Rc<Self>, result: Result<cairn_core::verify::VerifyReport, String>) {
        self.end_run();
        match result {
            Ok(report) if report.is_clean() => self.toast(&format!(
                "All {} files match what was extracted.",
                report.checked
            )),
            Ok(report) => self.toast(&format!(
                "{} files differ, {} are missing. Press Update to repair them.",
                report.mismatched.len(),
                report.missing.len()
            )),
            Err(e) => self.toast(&e),
        }
    }

    fn end_run(self: &Rc<Self>) {
        self.busy.set(false);
        self.running.borrow_mut().take();
        self.activity.set_reveal_child(false);
        self.progress.set_fraction(0.0);
        self.refresh();
    }

    // -----------------------------------------------------------------------
    // Settings and about
    // -----------------------------------------------------------------------

    fn open_settings(self: &Rc<Self>) {
        let dialog = adw::PreferencesDialog::new();
        dialog.set_title("Settings");

        let page = adw::PreferencesPage::new();
        page.set_title("Extraction");
        page.set_icon_name(Some("emblem-system-symbolic"));

        // --- language -----------------------------------------------------
        let group = adw::PreferencesGroup::new();
        group.set_title("Language");
        group.set_description(Some(
            "D2R ships dubbed audio and localised text for these. Choosing one replaces the \
             English files with that language's — it is not an extra copy.",
        ));

        let names: Vec<&str> = std::iter::once("English only (no localised files)")
            .chain(LANGUAGES.iter().map(|(_, name)| *name))
            .collect();
        let string_list = gtk::StringList::new(&names);
        // A plain preference row with a dropdown in it: ComboBoxRow gives the same
        // thing but takes its model through a different API, and this needs the
        // selected index to mean "no language at 0".
        let combo = adw::ActionRow::new();
        combo.set_title("Localised files");
        let model = gtk::DropDown::new(Some(string_list), gtk::Expression::NONE);
        model.set_valign(gtk::Align::Center);
        combo.add_suffix(&model);

        let current = self.config.borrow().language.clone();
        let selected = current
            .as_deref()
            .and_then(|code| {
                LANGUAGES
                    .iter()
                    .position(|(c, _)| c.eq_ignore_ascii_case(code))
            })
            .map(|i| i as u32 + 1)
            .unwrap_or(0);
        model.set_selected(selected);

        let this = Rc::clone(self);
        model.connect_selected_notify(move |dropdown| {
            let index = dropdown.selected();
            let language = if index == 0 {
                None
            } else {
                LANGUAGES
                    .get(index as usize - 1)
                    .map(|(code, _)| (*code).to_string())
            };
            this.config.borrow_mut().language = language;
            this.save();
        });
        group.add(&combo);
        page.add(&group);

        // --- verify -------------------------------------------------------
        let verify_group = adw::PreferencesGroup::new();
        verify_group.set_title("Updates");
        let verify_row = adw::SwitchRow::new();
        verify_row.set_title("Verify extracted file contents during Update");
        verify_row.set_subtitle(
            "Checks every file against its record instead of trusting size and content key. \
             Catches files damaged or edited outside Cairn, and reads the whole extraction, so \
             it takes several extra minutes.",
        );
        verify_row.set_active(self.config.borrow().verify_contents);
        let this = Rc::clone(self);
        verify_row.connect_active_notify(move |row| {
            this.config.borrow_mut().verify_contents = row.is_active();
            this.save();
        });
        verify_group.add(&verify_row);
        page.add(&verify_group);

        dialog.add(&page);
        dialog.present(Some(&self.window));
    }

    fn open_about(self: &Rc<Self>) {
        let about = adw::AboutDialog::builder()
            .application_name(cairn_core::APP_NAME)
            .application_icon(cairn_core::APP_ID)
            .version(cairn_core::VERSION)
            .developer_name("Ferah")
            .developers(vec!["Ferah"])
            .comments(
                "Extracts Diablo II: Resurrected's data archives into plain files and keeps a \
                 record of exactly what it wrote, so a patch only rewrites what changed and undo \
                 removes only what Cairn created.\n\nAdd -direct -txt to the game's launch \
                 options after extracting.\n\nArchive reading by CascLib, by Ladislav Zezula. The \
                 feature set follows D2R File Extractor by levinium.",
            )
            .license_type(gtk::License::MitX11)
            .build();
        about.present(Some(&self.window));
    }

    // -----------------------------------------------------------------------

    fn toast(&self, message: &str) {
        let toast = adw::Toast::new(message);
        toast.set_timeout(5);
        self.toast_overlay.add_toast(toast);
    }
}

/// A menu item bound to a per-row action, with its target as an explicit uint32.
fn menu_item(label: &str, action: &str, index: usize) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(&(index as u32).to_variant()));
    item
}

/// Fills the destinations dialog's list from the config, and again after any edit.
///
/// A free function rather than a closure so its children can call it: a closure that
/// has to call itself needs a `RefCell<Option<Rc<dyn Fn()>>>` dance that is pure
/// noise for four rows.
fn fill_destinations(this: &Rc<CairnWindow>, index: usize, list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    let installation = {
        let config = this.config.borrow();
        match config.installations.get(index) {
            Some(installation) => installation.clone(),
            None => return,
        }
    };
    let game_folder = installation.path.clone();

    for destination in &installation.destinations {
        let row = adw::ActionRow::new();
        let is_game = destination.path == game_folder;
        row.set_title(&glib::markup_escape_text(&destination.display_name()));
        row.set_subtitle(&glib::markup_escape_text(&format!(
            "{}{}",
            manifest::describe(&destination.path),
            if is_game {
                "  ·  this is the game folder, the one -direct reads"
            } else {
                ""
            }
        )));

        let switch = gtk::Switch::builder()
            .active(destination.enabled)
            .valign(gtk::Align::Center)
            .tooltip_text("Write to this destination")
            .build();
        let path = destination.path.clone();
        let this_switch = Rc::clone(this);
        let list_switch = list.clone();
        switch.connect_active_notify(move |switch| {
            if let Some(installation) = this_switch.config.borrow_mut().installations.get_mut(index)
            {
                for d in installation.destinations.iter_mut() {
                    if d.path == path {
                        d.enabled = switch.is_active();
                    }
                }
            }
            this_switch.save();
            fill_destinations(&this_switch, index, &list_switch);
        });
        row.add_suffix(&switch);

        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("Stop writing here — files already written are kept")
            .build();
        remove.add_css_class("flat");
        remove.set_sensitive(!is_game);
        let path = destination.path.clone();
        let this_remove = Rc::clone(this);
        let list_remove = list.clone();
        remove.connect_clicked(move |_| {
            if let Some(installation) = this_remove.config.borrow_mut().installations.get_mut(index)
            {
                installation.remove_destination(&path);
            }
            this_remove.save();
            fill_destinations(&this_remove, index, &list_remove);
        });
        row.add_suffix(&remove);

        list.append(&row);
    }
}

/// A destination's label for logs, kept tiny so the row code stays readable.
#[allow(dead_code)]
fn destination_label(destination: &Destination) -> String {
    destination.display_name()
}
