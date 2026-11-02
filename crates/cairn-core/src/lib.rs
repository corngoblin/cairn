//! Cairn — the D2R data extractor for Linux.
//!
//! Diablo II: Resurrected reads its art, sound and data out of compressed CASC
//! archives every time it loads a zone. Decompressing them is where the loading
//! time goes. Unpack them into plain files and point the game at them with
//! `-direct -txt`, and it reads instead of decompressing.
//!
//! This crate is that, and nothing else. It is a faithful port of the feature set
//! of [D2R File Extractor](https://github.com/levinium/D2RExtractor) — which does
//! the same job on Windows — onto CascLib on Linux, with the same guarantees:
//!
//! * a **manifest** per destination, recording every file written and the content
//!   key the archive keeps for it
//! * **update** that writes only what a patch changed, using those keys rather
//!   than reading 45 GB back
//! * **undo** that removes exactly the files Cairn wrote, and nothing the user put
//!   there
//! * **destinations**, so one extraction can feed the game folder and a mods folder
//! * **localised** files that replace the English ones they are dubbed over
//!
//! # Layers
//!
//! ```text
//!   casc-sys      CascLib FFI: handles, enumeration, file reads
//!   cairn-core    <- this crate: paths, planning, manifest, engine, detection
//!   cairn-cli     headless commands, for scripting and for testing without a display
//!   cairn-app     the GTK4 / libadwaita window
//! ```
//!
//! Everything above [`storage`] is testable without a game install:
//! [`storage::FixtureSource`] presents archive-shaped virtual paths backed by files
//! in a temporary directory, so the planning and undo logic run for real in
//! `cargo test`.

pub mod detect;
pub mod disk;
pub mod engine;
pub mod manifest;
pub mod paths;
pub mod plan;
pub mod progress;
pub mod settings;
pub mod storage;
pub mod util;
pub mod verify;

pub use engine::{
    run, undo, Change, ChangeKind, RunError, RunKind, RunOptions, RunSummary, UndoSummary,
};
pub use manifest::{Entry, Manifest};
pub use plan::{Plan, PlanOptions, WriteReason};
pub use progress::{Phase, Progress, ProgressSink};
pub use settings::{Config, Destination, Installation};
pub use storage::{CascSource, Source, SourceEntry, SourceError};

/// The app's identity, in one place so the window title, the desktop file and the
/// flatpak id cannot drift apart.
pub const APP_ID: &str = "io.github.corngoblin.Cairn";
pub const APP_NAME: &str = "Cairn";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Runs an extraction or update into every active destination of `installation`.
///
/// Enumeration happens once, in the source the caller built; each destination is
/// then planned and written independently, and each keeps its own manifest. A
/// cancelled run stops the loop; a failed one is reported against its destination
/// and the others continue, because one unwritable mods folder should not prevent
/// the game folder from being updated.
pub fn run_all_destinations(
    source: &dyn Source,
    installation: &Installation,
    options: &RunOptions,
    progress: &ProgressSink<'_>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Vec<Result<(std::path::PathBuf, RunSummary), (std::path::PathBuf, RunError)>> {
    let mut results = Vec::new();
    for destination in installation.active_destinations() {
        match engine::run(source, &destination.path, options, progress, cancel) {
            Ok(summary) => results.push(Ok((destination.path.clone(), summary))),
            Err(e) => {
                let fatal = matches!(e, RunError::Cancelled);
                results.push(Err((destination.path.clone(), e)));
                if fatal {
                    break;
                }
            }
        }
    }
    results
}
