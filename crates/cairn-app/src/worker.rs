//! Running extractions off the main loop, and getting the news back to the window.
//!
//! GTK is single-threaded and CascLib is blocking, so every long operation happens
//! on a worker thread and reports progress over a channel. The window drains that
//! channel on a 100 ms timer instead of waking itself for every one of 150k files:
//! one timer, one place messages can arrive, nothing to get wrong with `unsafe`
//! `Send` wrappers.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use cairn_core::engine::{self, RunError, RunSummary, UndoSummary};
use cairn_core::progress::{Progress, ProgressSink};
use cairn_core::settings::Installation;
use cairn_core::storage::{CascSource, Source};
use cairn_core::verify::{self, VerifyReport};

/// A message from a worker to the window.
pub enum Msg {
    Progress(Progress),
    Log(String),
    /// An extract/update finished. One entry per destination.
    Extracted(Vec<Result<(PathBuf, RunSummary), (PathBuf, RunError)>>),
    Undone(Result<UndoSummary, RunError>),
    Verified(Result<VerifyReport, String>),
    Failed(String),
}

/// The handle the window keeps for the run in flight.
pub struct Running {
    pub cancel: Arc<AtomicBool>,
    pub rx: Receiver<Msg>,
}

impl Running {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn channel() -> (Sender<Msg>, Running) {
    let (tx, rx) = mpsc::channel();
    (
        tx,
        Running {
            cancel: Arc::new(AtomicBool::new(false)),
            rx,
        },
    )
}

/// Extracts or updates every active destination of an installation.
pub fn spawn_extract(
    installation: Installation,
    language: Option<String>,
    verify_contents: bool,
) -> Running {
    let (tx, running) = channel();
    let cancel = running.cancel.clone();

    std::thread::spawn(move || {
        let log = |message: String| {
            let _ = tx.send(Msg::Log(message));
        };
        let report = |p: &Progress| {
            let _ = tx.send(Msg::Progress(p.clone()));
        };
        let progress = ProgressSink::new(&report);

        let destinations: Vec<PathBuf> = installation
            .active_destinations()
            .iter()
            .map(|d| d.path.clone())
            .collect();
        if destinations.is_empty() {
            let _ = tx.send(Msg::Failed(
                "No destination is enabled for this installation.".to_string(),
            ));
            return;
        }

        log(format!(
            "Opening the archives in {}",
            installation.path.display()
        ));

        let source =
            match CascSource::open(&installation.path, language.as_deref(), &progress, &cancel) {
                Ok(source) => source,
                Err(cairn_core::SourceError::Cancelled) => {
                    let _ = tx.send(Msg::Failed("Cancelled.".to_string()));
                    return;
                }
                Err(e) => {
                    // The toast has one line and gets ellipsised, so it carries the part
                    // the user can act on and the detail goes to the log.
                    log(format!("{e}"));
                    let _ = tx.send(Msg::Failed(format!(
                    "Could not read the archives in {}. Is this the D2R folder — the one with a \
                     Data folder inside?",
                    installation.path.display()
                )));
                    return;
                }
            };

        let entries = source.entries();
        let available = entries.iter().filter(|e| e.available).count();
        log(format!(
            "{} files in the archives{}",
            entries.len(),
            if available < entries.len() {
                format!(" ({available} present on this disk)")
            } else {
                String::new()
            }
        ));

        let options = engine::RunOptions {
            international: language,
            plan: cairn_core::PlanOptions { verify_contents },
        };

        let mut results = Vec::new();
        for destination in &destinations {
            log(format!("Writing to {}", destination.display()));
            match engine::run(&source, destination, &options, &progress, &cancel) {
                Ok(summary) => results.push(Ok((destination.clone(), summary))),
                Err(e) => {
                    let cancelled = matches!(e, RunError::Cancelled);
                    results.push(Err((destination.clone(), e)));
                    if cancelled {
                        break;
                    }
                }
            }
        }
        let _ = tx.send(Msg::Extracted(results));
    });

    running
}

/// Removes everything recorded for a destination.
pub fn spawn_undo(destination: PathBuf) -> Running {
    let (tx, running) = channel();
    let cancel = running.cancel.clone();

    std::thread::spawn(move || {
        let report = |p: &Progress| {
            let _ = tx.send(Msg::Progress(p.clone()));
        };
        let progress = ProgressSink::new(&report);
        let _ = tx.send(Msg::Log(format!(
            "Removing what Cairn wrote to {}",
            destination.display()
        )));
        let result = engine::undo(&destination, &progress, &cancel);
        let _ = tx.send(Msg::Undone(result));
    });

    running
}

/// Hashes a destination's files and compares them with its record.
pub fn spawn_verify(destination: PathBuf) -> Running {
    let (tx, running) = channel();
    let cancel = running.cancel.clone();

    std::thread::spawn(move || {
        let report = |p: &Progress| {
            let _ = tx.send(Msg::Progress(p.clone()));
        };
        let progress = ProgressSink::new(&report);
        let result = (|| {
            let manifest = cairn_core::manifest::Manifest::load(&destination)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("nothing recorded in {}", destination.display()))?;
            Ok(verify::verify(&destination, &manifest, &progress, &cancel))
        })();
        let _ = tx.send(Msg::Verified(result));
    });

    running
}
