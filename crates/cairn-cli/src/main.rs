//! `cairn` — the headless side of the extractor.
//!
//! The window is the product, but this is the half that makes it possible to test,
//! to debug a stranger's installation from a shell, and to script a nightly "update
//! if the game patched" job. Everything the window does is one of these commands
//! underneath.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use cairn_core::engine;
use cairn_core::manifest::Manifest;
use cairn_core::progress::{Progress, ProgressSink};
use cairn_core::settings::Config;
use cairn_core::storage::{CascSource, Source};
use cairn_core::util;
use cairn_core::verify;

#[derive(Parser)]
#[command(
    name = "cairn",
    version = cairn_core::VERSION,
    about = "Extract Diablo II: Resurrected's data files so the game loads from disk",
    long_about = "Cairn unpacks D2R's CASC archives into plain files and keeps a record of \
                  exactly what it wrote, so a patch updates only what changed and Undo removes \
                  only what Cairn created.\n\nAfter extracting, add `-direct -txt` to the game's \
                  launch options — without them the game ignores the files entirely."
)]
struct Cli {
    /// Config file override. Defaults to $XDG_CONFIG_HOME/cairn/config.json.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the installations configured, and what is extracted where.
    Status,
    /// Look for D2R installations on this machine, and optionally add them.
    Scan {
        /// Add what is found to the config. Without this, only report.
        #[arg(long)]
        add: bool,
    },
    /// Open a game folder's archives and describe what is inside.
    Inspect {
        /// The installation folder (the one containing the Data folder).
        install: PathBuf,
        /// Skip the full enumeration and only report that the storage opens.
        #[arg(long)]
        quick: bool,
    },
    /// Extract into plain files, or update an existing extraction.
    Extract {
        /// The installation folder to read from.
        install: PathBuf,
        /// Where to write. Repeatable. Defaults to the installation folder itself.
        #[arg(long = "to")]
        destinations: Vec<PathBuf>,
        /// Language code whose localised files replace the English ones (e.g. itIT).
        #[arg(long)]
        lang: Option<String>,
        /// Hash every recorded file instead of trusting size and content key.
        #[arg(long)]
        verify: bool,
        /// Show what would be done, then stop without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove what an extraction wrote, and only that.
    Undo {
        /// The destination Cairn extracted into.
        #[arg(default_value = ".")]
        destination: PathBuf,
    },
    /// Hash extracted files and compare them with the record.
    Verify {
        #[arg(default_value = ".")]
        destination: PathBuf,
    },
    /// List the files an extraction touched last time.
    Changes {
        #[arg(default_value = ".")]
        destination: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or_else(Config::default_path);
    let mut config = Config::load(&config_path);

    let result = match cli.command {
        Command::Status => status(&config, &config_path),
        Command::Scan { add } => scan(&mut config, &config_path, add),
        Command::Inspect { install, quick } => inspect(&install, quick),
        Command::Extract {
            install,
            destinations,
            lang,
            verify,
            dry_run,
        } => extract(&install, destinations, lang, verify, dry_run),
        Command::Undo { destination } => undo(&destination),
        Command::Verify { destination } => verify_destination(&destination),
        Command::Changes { destination } => changes(&destination),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cairn: {e:#}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// status / scan
// ---------------------------------------------------------------------------

fn status(config: &Config, config_path: &Path) -> Result<()> {
    println!("cairn {}", cairn_core::VERSION);
    println!("config: {}", config_path.display());
    if config.installations.is_empty() {
        println!("\nNo installations configured. Run `cairn scan` to look for one.");
        return Ok(());
    }

    for installation in &config.installations {
        let exists = installation.path.is_dir();
        println!(
            "\n{} {}",
            if exists { "*" } else { "-" },
            installation.display_name()
        );
        if !exists {
            println!("    folder is gone");
            continue;
        }
        if !cairn_core::detect::is_installation(&installation.path) {
            println!("    no game archives found here");
        }
        for destination in installation.active_destinations() {
            println!("    -> {}", destination.display_name());
            println!(
                "       {}",
                cairn_core::manifest::describe(&destination.path)
            );
        }
    }

    if let Some(lang) = &config.language {
        println!("\nlanguage: {lang} (localised files replace English)");
    }
    if config.verify_contents {
        println!("verify contents on update: yes");
    }
    Ok(())
}

fn scan(config: &mut Config, config_path: &Path, add: bool) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let found = cairn_core::detect::find_installations(&home);
    if found.is_empty() {
        println!("No D2R installation found. Add one with `cairn extract <folder>`.");
        return Ok(());
    }
    for path in &found {
        let known = config.installations.iter().any(|i| &i.path == path);
        println!(
            "{}{}",
            path.display(),
            if known { "  (already known)" } else { "" }
        );
    }
    if add {
        let added = config.discover(&home);
        config.save(config_path)?;
        println!(
            "\nadded {added} installation(s) to {}",
            config_path.display()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

fn inspect(install: &Path, quick: bool) -> Result<()> {
    if !install.is_dir() {
        bail!("{} is not a folder", install.display());
    }
    println!("opening {}", install.display());
    for marker in [
        ".build.info",
        ".build.db",
        "versions",
        "data/.build.config",
        "Data/.build.config",
    ] {
        println!(
            "  [{}] {}",
            if install.join(marker).exists() {
                "found"
            } else {
                "absent"
            },
            marker
        );
    }

    if quick {
        match casc_sys::Storage::open(install) {
            Ok(storage) => {
                println!(
                    "\nstorage opens at {}{}",
                    storage.opened_at().display(),
                    storage
                        .product()
                        .map(|p| format!(" (product: {p})"))
                        .unwrap_or_default()
                );
            }
            Err(e) => bail!("{e}"),
        }
        return Ok(());
    }

    let cancel = AtomicBool::new(false);
    let sink = ProgressSink::new(&|p: &Progress| {
        if let Some(f) = p.fraction() {
            eprint!("\r{}: {:.0}% {}", p.phase.label(), f * 100.0, p.current);
        } else {
            eprint!("\r{}: {}", p.phase.label(), p.current);
        }
    });
    let source = CascSource::open(install, None, &sink, &cancel)?;
    eprintln!();

    let entries = source.entries();
    let available = entries.iter().filter(|e| e.available).count();
    let bytes: u64 = entries.iter().filter(|e| e.available).map(|e| e.size).sum();
    let with_keys = entries.iter().filter(|e| e.ckey.is_some()).count();

    println!("\n{}", source.describe());
    println!("  files:          {}", entries.len());
    println!("  available here: {available}");
    println!("  content keys:   {with_keys}");
    println!("  size on disk:   {}", util::format_bytes(bytes));

    let mut by_tree: std::collections::BTreeMap<String, usize> = Default::default();
    for entry in entries {
        let key = entry.key();
        let tree = key.split('/').take(2).collect::<Vec<_>>().join("/");
        *by_tree.entry(tree).or_default() += 1;
    }
    println!("  by tree:");
    for (tree, count) in by_tree {
        println!("    {tree:<24} {count:>8}");
    }

    println!("  first few files:");
    for entry in entries.iter().take(5) {
        println!("    {} ({} bytes)", entry.key(), entry.size);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// extract
// ---------------------------------------------------------------------------

fn extract(
    install: &Path,
    destinations: Vec<PathBuf>,
    lang: Option<String>,
    verify_contents: bool,
    dry_run: bool,
) -> Result<()> {
    if !install.is_dir() {
        bail!("{} is not a folder", install.display());
    }

    let destinations: Vec<PathBuf> = if destinations.is_empty() {
        vec![install.to_path_buf()]
    } else {
        destinations
    };

    let cancel = AtomicBool::new(false);
    let sink = ProgressSink::new(&|p: &Progress| {
        if let Some(f) = p.fraction() {
            eprint!(
                "\r{} {:>3.0}%  {}/{}  {}  {}",
                p.phase.label(),
                f * 100.0,
                p.files_done,
                p.files_total,
                util::format_bytes(p.bytes_done),
                truncate(&p.current, 48)
            );
        } else {
            eprint!("\r{}: {}", p.phase.label(), truncate(&p.current, 60));
        }
    });

    println!("Reading {}", install.display());
    let source = CascSource::open(install, lang.as_deref(), &sink, &cancel)?;
    eprintln!();
    println!("{} files in {}", source.entries().len(), source.describe());

    let options = engine::RunOptions {
        international: lang.clone(),
        plan: cairn_core::PlanOptions { verify_contents },
    };

    let mut failures = 0;
    for destination in &destinations {
        println!("\n{}", destination.display());

        if dry_run {
            let existing = Manifest::load(destination)?;
            let plan =
                cairn_core::plan::build(&source, destination, existing.as_ref(), options.plan)?;
            if plan.is_empty() {
                println!("  nothing to do — already in line with the archives");
                continue;
            }
            println!(
                "  would write {} files, {}",
                plan.write_files(),
                util::format_bytes(plan.write_bytes)
            );
            let mut reasons: std::collections::BTreeMap<&str, usize> = Default::default();
            for write in &plan.writes {
                *reasons.entry(write.reason.label()).or_default() += 1;
            }
            for (reason, count) in reasons {
                println!("    {reason}: {count}");
            }
            println!("  would remove {} stale files", plan.deletes.len());
            continue;
        }

        match engine::run(&source, destination, &options, &sink, &cancel) {
            Ok(summary) => {
                eprintln!();
                match summary.kind {
                    engine::RunKind::UpToDate => println!(
                        "  already up to date — {} files unchanged",
                        summary.unchanged
                    ),
                    kind => println!(
                        "  {} finished: {} written ({}), {} removed, {} unchanged, {}",
                        kind.label(),
                        summary.written,
                        util::format_bytes(summary.bytes_written),
                        summary.removed,
                        summary.unchanged,
                        util::format_duration(summary.elapsed_secs)
                    ),
                }
                if summary.unavailable > 0 {
                    println!(
                        "  {} file(s) are in the archives but not on this disk (CDN-only); they \
                         were skipped and will be retried on the next run",
                        summary.unavailable
                    );
                }
            }
            Err(e) => {
                eprintln!();
                eprintln!("  failed: {e:#}");
                failures += 1;
            }
        }
    }

    if failures > 0 {
        bail!("{failures} destination(s) failed");
    }
    println!("\nDone. Add -direct -txt to the game's launch options to use the extracted files.");
    Ok(())
}

// ---------------------------------------------------------------------------
// undo / verify / changes
// ---------------------------------------------------------------------------

fn undo(destination: &Path) -> Result<()> {
    let cancel = AtomicBool::new(false);
    let sink = ProgressSink::new(&|p: &Progress| {
        eprint!(
            "\r{}: {} / {}",
            p.phase.label(),
            p.files_done,
            p.files_total
        );
    });
    let summary = engine::undo(destination, &sink, &cancel)?;
    eprintln!();
    if summary.is_noop() {
        println!("nothing to undo in {}", destination.display());
        return Ok(());
    }
    println!(
        "removed {} files ({} already gone, {} could not be removed)",
        summary.removed, summary.missing, summary.kept
    );
    println!(
        "Take -direct -txt back out of the launch options; the game reads its archives again."
    );
    Ok(())
}

fn verify_destination(destination: &Path) -> Result<()> {
    let Some(manifest) = Manifest::load(destination)? else {
        bail!("no extraction recorded in {}", destination.display());
    };
    let cancel = AtomicBool::new(false);
    let sink = ProgressSink::new(&|p: &Progress| {
        eprint!(
            "\r{}: {} / {}",
            p.phase.label(),
            p.files_done,
            p.files_total
        );
    });
    let report = verify::verify(destination, &manifest, &sink, &cancel);
    eprintln!();

    println!(
        "checked {} files: {} match, {} differ, {} missing, {} unverifiable",
        report.checked,
        report.ok,
        report.mismatched.len(),
        report.missing.len(),
        report.unverifiable
    );
    for path in report.mismatched.iter().take(20) {
        println!("  differs: {}", path.display());
    }
    for path in report.missing.iter().take(20) {
        println!("  missing: {}", path.display());
    }
    if !report.is_clean() {
        println!("\nRun `cairn extract` on this installation to repair them.");
    }
    Ok(())
}

fn changes(destination: &Path) -> Result<()> {
    let Some(changes) = engine::load_change_log(destination) else {
        println!("no change list recorded in {}", destination.display());
        return Ok(());
    };
    for change in &changes {
        println!(
            "{:>9}  {}  {}",
            format!("{:?}", change.change).to_lowercase(),
            util::format_bytes(change.size),
            change.path
        );
    }
    println!("\n{} change(s)", changes.len());
    Ok(())
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        let mut s: String = text.chars().take(width.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}
