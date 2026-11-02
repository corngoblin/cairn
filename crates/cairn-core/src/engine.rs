//! Running a plan: writing files, removing leftovers, undoing the whole thing.
//!
//! The loop is deliberately boring, because what happens when it is *interrupted*
//! is the part that matters. Three properties are load-bearing:
//!
//! 1. The manifest is saved, marked incomplete, **before** the first file is
//!    written. A crash one second later still leaves a record naming nothing —
//!    which is correct — rather than files on disk with no record at all.
//! 2. Each file is written to a temporary name and renamed into place. A file with
//!    the final name is therefore a complete file, so an interrupted run never
//!    leaves a truncated asset that looks extracted.
//! 3. The manifest is flushed periodically during the run, so a crash loses at most
//!    the last few thousand entries rather than the whole record. On a 40-minute
//!    extraction that is the difference between "press Extract again" and
//!    "45 GB of files nobody can undo".

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::manifest::{Entry, KeySource, Manifest};
use crate::plan::{self, PlanOptions, WriteReason};
use crate::progress::{Phase, Progress, ProgressSink, Throttle};
use crate::storage::Source;
use crate::util::now_unix;

/// Suffix used for a file being written. Kept short because it is appended to
/// ~150k names.
const PART_SUFFIX: &str = ".cairn-part";
/// How often the manifest is flushed mid-run.
const FLUSH_EVERY_FILES: u64 = 2_000;
const FLUSH_EVERY_SECONDS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunKind {
    /// Nothing existed; everything was written.
    Extract,
    /// Something was already extracted; only the differences were written.
    Update,
    /// Already in line with the archives; nothing was touched.
    UpToDate,
}

impl RunKind {
    pub fn label(self) -> &'static str {
        match self {
            RunKind::Extract => "Extraction",
            RunKind::Update => "Update",
            RunKind::UpToDate => "Already up to date",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunSummary {
    pub kind: RunKind,
    pub written: u64,
    pub bytes_written: u64,
    pub removed: u64,
    pub unchanged: u64,
    /// Files the archives list but could not provide — CDN-only entries, offline.
    pub unavailable: u64,
    pub elapsed_secs: u64,
    /// The individual changes, capped (see [`CHANGE_LOG_CAP`]).
    pub changes: Vec<Change>,
    pub changes_truncated: bool,
}

/// One file this run touched, for the "what did that patch actually do" view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    /// Relative path, `/`-separated.
    pub path: String,
    pub change: ChangeKind,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeKind {
    Added,
    Replaced,
    Removed,
}

/// Cap on the per-file change list written to disk. A fresh extraction's list
/// would be a second copy of the manifest, so it gets a summary instead.
pub const CHANGE_LOG_CAP: usize = 20_000;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Manifest(#[from] crate::manifest::ManifestError),
    #[error("cancelled")]
    Cancelled,
}

/// What a run is asked to do.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// The language whose localised files should replace the English ones.
    pub international: Option<String>,
    pub plan: PlanOptions,
}

/// Extracts or updates `destination` from `source`.
///
/// This is the single entry point for both: which one it turns out to be is a
/// consequence of what the manifest says, not of a flag the caller passes.
pub fn run(
    source: &dyn Source,
    destination: &Path,
    options: &RunOptions,
    progress: &ProgressSink<'_>,
    cancel: &AtomicBool,
) -> Result<RunSummary, RunError> {
    let started = Instant::now();
    let existing = Manifest::load(destination)?;

    progress.report(&Progress::enumerating("comparing with what is on disk…"));
    let plan = plan::build(source, destination, existing.as_ref(), options.plan)?;

    if plan.is_empty() && existing.as_ref().is_some_and(|m| m.complete) {
        return Ok(RunSummary {
            kind: RunKind::UpToDate,
            written: 0,
            bytes_written: 0,
            removed: 0,
            unchanged: plan.unchanged,
            unavailable: 0,
            elapsed_secs: started.elapsed().as_secs(),
            changes: Vec::new(),
            changes_truncated: false,
        });
    }

    let kind = if plan.is_fresh() && existing.is_none() {
        RunKind::Extract
    } else {
        RunKind::Update
    };

    // Start from the previous record so an interrupted update is still undoable
    // as a whole; entries for files that vanish are dropped after they are deleted.
    let mut manifest = existing.unwrap_or_else(|| Manifest::new(source.root_path()));
    manifest.storage_path = source.root_path().to_path_buf();
    manifest.international = options.international.clone();
    manifest.complete = false;
    manifest.incomplete_reason = None;
    if plan.writes.iter().all(|w| w.entry.ckey.is_none()) && !plan.writes.is_empty() {
        manifest.key_source = KeySource::None;
    }
    manifest.save(destination)?;

    let files_total = plan.write_files() as u64;
    let bytes_total = plan.write_bytes;
    let mut summary = RunSummary {
        kind,
        written: 0,
        bytes_written: 0,
        removed: 0,
        unchanged: plan.unchanged,
        unavailable: 0,
        elapsed_secs: 0,
        changes: Vec::new(),
        changes_truncated: false,
    };

    let throttle = Throttle::new(120);
    let mut since_flush = 0u64;
    let mut last_flush = Instant::now();

    for (index, write) in plan.writes.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            // Leave what is on disk in a consistent state: the manifest already
            // names everything written so far, and stays marked incomplete.
            manifest.incomplete_reason = Some("cancelled".to_string());
            manifest.save(destination)?;
            return Err(RunError::Cancelled);
        }

        let target = destination.join(&write.entry.rel_path);
        if let Some(parent) = target.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                manifest.incomplete_reason = Some(format!("{}: {e}", parent.display()));
                let _ = manifest.save(destination);
                return Err(RunError::Io(e));
            }
        }

        match write_one(source, &write.entry.virtual_path, &target) {
            Ok(Some(bytes)) => {
                summary.written += 1;
                summary.bytes_written += bytes;
                manifest.entries.insert(
                    write.entry.key(),
                    Entry {
                        size: bytes,
                        ckey: write.entry.ckey.clone(),
                    },
                );
                if summary.changes.len() < CHANGE_LOG_CAP {
                    summary.changes.push(Change {
                        path: write.entry.key(),
                        change: if write.reason == WriteReason::New {
                            ChangeKind::Added
                        } else {
                            ChangeKind::Replaced
                        },
                        size: bytes,
                    });
                } else {
                    summary.changes_truncated = true;
                }
            }
            Ok(None) => {
                // Listed in the archives, not obtainable from this storage. Do not
                // record it: a later run that can reach it will retry.
                summary.unavailable += 1;
            }
            Err(e) => {
                manifest.incomplete_reason = Some(format!("{}: {e}", write.entry.key()));
                let _ = manifest.save(destination);
                return Err(RunError::Io(e));
            }
        }

        if throttle.ready() {
            progress.report(&Progress {
                phase: Phase::Writing,
                files_done: index as u64 + 1,
                files_total,
                bytes_done: summary.bytes_written,
                bytes_total,
                current: write.entry.key(),
            });
        }

        since_flush += 1;
        if since_flush >= FLUSH_EVERY_FILES || last_flush.elapsed().as_secs() >= FLUSH_EVERY_SECONDS
        {
            manifest.save(destination)?;
            since_flush = 0;
            last_flush = Instant::now();
        }
    }

    // Deletions come after the writes: if the run dies between the two, the
    // destination holds both the old and the new files, which the game survives,
    // rather than neither.
    if !plan.deletes.is_empty() {
        let remove_total = plan.deletes.len() as u64;
        let mut removed_dirs: Vec<PathBuf> = Vec::new();
        for (index, delete) in plan.deletes.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                manifest.incomplete_reason = Some("cancelled".to_string());
                manifest.save(destination)?;
                return Err(RunError::Cancelled);
            }
            let path = destination.join(&delete.rel_path);
            match fs::remove_file(&path) {
                Ok(()) => {
                    summary.removed += 1;
                    manifest
                        .entries
                        .remove(&delete.rel_path.to_string_lossy().replace('\\', "/"));
                    if summary.changes.len() < CHANGE_LOG_CAP {
                        summary.changes.push(Change {
                            path: delete.rel_path.to_string_lossy().replace('\\', "/"),
                            change: ChangeKind::Removed,
                            size: delete.size,
                        });
                    } else {
                        summary.changes_truncated = true;
                    }
                    if let Some(parent) = path.parent() {
                        removed_dirs.push(parent.to_path_buf());
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    // Already gone; the record was stale. Just forget it.
                    manifest
                        .entries
                        .remove(&delete.rel_path.to_string_lossy().replace('\\', "/"));
                }
                Err(e) => {
                    manifest.incomplete_reason = Some(format!("{}: {e}", path.display()));
                    let _ = manifest.save(destination);
                    return Err(RunError::Io(e));
                }
            }
            if throttle.ready() || index + 1 == plan.deletes.len() {
                progress.report(&Progress {
                    phase: Phase::Removing,
                    files_done: index as u64 + 1,
                    files_total: remove_total,
                    bytes_done: index as u64 + 1,
                    bytes_total: remove_total,
                    current: delete.rel_path.display().to_string(),
                });
            }
        }
        prune_empty_dirs(destination, &mut removed_dirs);
    }

    manifest.complete = true;
    manifest.incomplete_reason = if summary.unavailable > 0 {
        Some(format!(
            "{} file(s) in the archives could not be read from this storage",
            summary.unavailable
        ))
    } else {
        None
    };
    manifest.save(destination)?;

    summary.elapsed_secs = started.elapsed().as_secs();
    write_change_log(destination, &summary);
    Ok(summary)
}

/// Writes one file via a temporary name, so the final name only ever refers to a
/// complete file.
fn write_one(source: &dyn Source, virtual_path: &str, target: &Path) -> io::Result<Option<u64>> {
    let tmp = target.with_file_name(format!(
        "{}{PART_SUFFIX}",
        target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string())
    ));

    let file = File::create(&tmp)?;
    let mut writer = BufWriter::with_capacity(256 * 1024, file);
    let written = match source.copy_to(virtual_path, &mut writer) {
        Ok(Some(n)) => n,
        Ok(None) => {
            drop(writer);
            let _ = fs::remove_file(&tmp);
            return Ok(None);
        }
        Err(e) => {
            drop(writer);
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    writer.flush()?;
    let file = writer.into_inner().map_err(|e| e.into_error())?;
    // The bytes are on the platter before the name says "complete".
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, target)?;
    Ok(Some(written))
}

/// Removes directories left empty by a delete pass, deepest first. A directory
/// that still has anything in it — including a file the user put there — stays.
fn prune_empty_dirs(destination: &Path, dirs: &mut Vec<PathBuf>) {
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    dirs.dedup();
    for dir in dirs {
        // Never walk above the destination itself.
        if !dir.starts_with(destination) || dir == destination {
            continue;
        }
        let mut current = dir.clone();
        while current.starts_with(destination) && current != destination {
            // remove_dir only succeeds on an empty directory, which is exactly the
            // "only contains what we deleted" test.
            if fs::remove_dir(&current).is_err() {
                break;
            }
            match current.parent() {
                Some(parent) => current = parent.to_path_buf(),
                None => break,
            }
        }
    }
}

/// Removes everything the manifest names, and the manifest itself.
///
/// Files the user added to the same folder are not in the manifest, so they are
/// not touched — which is the whole reason undo can be offered without a warning
/// dialog.
pub fn undo(
    destination: &Path,
    progress: &ProgressSink<'_>,
    cancel: &AtomicBool,
) -> Result<UndoSummary, RunError> {
    let Some(manifest) = Manifest::load(destination)? else {
        return Ok(UndoSummary {
            removed: 0,
            missing: 0,
            kept: 0,
            had_manifest: false,
        });
    };

    let total = manifest.entries.len() as u64;
    let mut summary = UndoSummary {
        removed: 0,
        missing: 0,
        kept: 0,
        had_manifest: true,
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    let throttle = Throttle::new(120);

    for (index, (key, _entry)) in manifest.entries.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(RunError::Cancelled);
        }
        let path = destination.join(key);
        match fs::remove_file(&path) {
            Ok(()) => {
                summary.removed += 1;
                if let Some(parent) = path.parent() {
                    dirs.push(parent.to_path_buf());
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => summary.missing += 1,
            Err(_) => summary.kept += 1,
        }
        if throttle.ready() {
            progress.report(&Progress {
                phase: Phase::Removing,
                files_done: index as u64 + 1,
                files_total: total,
                bytes_done: index as u64 + 1,
                bytes_total: total,
                current: key.clone(),
            });
        }
    }

    prune_empty_dirs(destination, &mut dirs);
    Manifest::remove(destination)?;
    Ok(summary)
}

#[derive(Debug, Clone)]
pub struct UndoSummary {
    pub removed: u64,
    pub missing: u64,
    /// Recorded, but could not be removed (permissions, or replaced by a directory).
    pub kept: u64,
    pub had_manifest: bool,
}

impl UndoSummary {
    pub fn is_noop(&self) -> bool {
        !self.had_manifest
    }
}

/// The last run's change list, as written beside the manifest. Only the most
/// recent run is kept.
pub fn load_change_log(destination: &Path) -> Option<Vec<Change>> {
    let path = destination
        .join(crate::manifest::STATE_DIR)
        .join("last-run.json");
    let text = fs::read_to_string(path).ok()?;
    let log: ChangeLog = serde_json::from_str(&text).ok()?;
    Some(log.changes)
}

#[derive(Debug, Serialize, Deserialize)]
struct ChangeLog {
    unix: i64,
    kind: RunKind,
    written: u64,
    removed: u64,
    changes: Vec<Change>,
    truncated: bool,
}

fn write_change_log(destination: &Path, summary: &RunSummary) {
    // A fresh extraction's list is the manifest again, so only its summary is kept.
    if summary.kind == RunKind::Extract && !summary.changes.is_empty() {
        return;
    }
    let log = ChangeLog {
        unix: now_unix(),
        kind: summary.kind,
        written: summary.written,
        removed: summary.removed,
        changes: summary.changes.clone(),
        truncated: summary.changes_truncated,
    };
    let path = destination
        .join(crate::manifest::STATE_DIR)
        .join("last-run.json");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec(&log) {
        let _ = fs::write(path, json);
    }
}

#[cfg(test)]
mod tests;
