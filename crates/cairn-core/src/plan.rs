//! Deciding what to write before writing anything.
//!
//! A full extraction writes ~150k files and ~45 GB. Re-running it after a game
//! patch must not do that again — a patch touches a few hundred files, and the
//! whole point of this tool is not to pay that cost twice. So the run is split:
//! first a plan, computed entirely from the archive index and the manifest, then
//! the writes the plan calls for.
//!
//! Two facts make the plan cheap:
//!
//! * The content key in the archive index **is** the fingerprint of the decoded
//!   content, and the manifest recorded the same key at extraction time. Comparing
//!   them tells us whether content changed without reading a single byte back:
//!   that is what `KeySource::CascCkey` buys.
//! * A recorded path that is not on disk is a write, regardless of keys. One
//!   `stat` per file, no hashing.
//!
//! Hashing the 45 GB on disk only happens when the user asks for it
//! (`verify`), because it is the only way to catch a file damaged outside the app,
//! and it costs minutes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::manifest::Manifest;
use crate::storage::{Source, SourceEntry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteReason {
    /// Not in the manifest — a first extraction or a file a patch added.
    New,
    /// Recorded, but no longer on disk.
    Missing,
    /// The archive says a different byte count than the manifest.
    SizeChanged,
    /// Same size, different content key: a patch rewrote the file.
    ContentChanged,
    /// Content key matches, but the bytes on disk hash to something else. Only
    /// reachable with `verify` on.
    Damaged,
}

impl WriteReason {
    pub fn label(&self) -> &'static str {
        match self {
            WriteReason::New => "new",
            WriteReason::Missing => "missing",
            WriteReason::SizeChanged => "size changed",
            WriteReason::ContentChanged => "content changed",
            WriteReason::Damaged => "damaged on disk",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlannedWrite {
    pub entry: SourceEntry,
    pub reason: WriteReason,
}

/// A recorded file the archives no longer contain — a leftover from an old patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedDelete {
    pub rel_path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub writes: Vec<PlannedWrite>,
    pub deletes: Vec<PlannedDelete>,
    pub unchanged: u64,
    pub write_bytes: u64,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty() && self.deletes.is_empty()
    }

    pub fn write_files(&self) -> usize {
        self.writes.len()
    }

    /// Whether this is a first extraction rather than an update.
    pub fn is_fresh(&self) -> bool {
        self.deletes.is_empty() && self.writes.iter().all(|w| w.reason == WriteReason::New)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PlanOptions {
    /// Hash every recorded file on disk instead of trusting size and key.
    pub verify_contents: bool,
}

/// Computes what has to change to bring `destination` in line with `source`.
///
/// `manifest` is `None` for a destination that has never been extracted into.
pub fn build(
    source: &dyn Source,
    destination: &Path,
    manifest: Option<&Manifest>,
    options: PlanOptions,
) -> std::io::Result<Plan> {
    let mut plan = Plan::default();

    // Keys the archives still provide. Anything recorded and absent from this set
    // is a leftover.
    let mut archive_keys: BTreeSet<String> = BTreeSet::new();

    for entry in source.entries() {
        let key = entry.key();
        archive_keys.insert(key.clone());

        let previous = manifest.and_then(|m| m.entries.get(&key));
        let reason = match previous {
            None => Some(WriteReason::New),
            Some(prev) => {
                let on_disk = destination.join(&entry.rel_path);
                if !on_disk.exists() {
                    Some(WriteReason::Missing)
                } else if prev.size != entry.size {
                    Some(WriteReason::SizeChanged)
                } else {
                    match (prev.ckey.as_deref(), entry.ckey.as_deref()) {
                        (Some(old), Some(new)) if old != new => Some(WriteReason::ContentChanged),
                        // No keys on one side or the other: the size is all there is
                        // to go on, unless the user asked for a real check.
                        _ if options.verify_contents => {
                            match crate::verify::hash_file(&on_disk) {
                                Some(actual) if Some(actual.as_str()) == entry.ckey.as_deref() => {
                                    None
                                }
                                Some(_) => Some(WriteReason::Damaged),
                                // Unreadable: let the write attempt report it.
                                None => Some(WriteReason::Damaged),
                            }
                        }
                        _ => None,
                    }
                }
            }
        };

        match reason {
            Some(reason) => {
                plan.write_bytes += entry.size;
                plan.writes.push(PlannedWrite {
                    entry: entry.clone(),
                    reason,
                });
            }
            None => plan.unchanged += 1,
        }
    }

    if let Some(manifest) = manifest {
        for (key, entry) in &manifest.entries {
            if !archive_keys.contains(key) {
                plan.deletes.push(PlannedDelete {
                    rel_path: PathBuf::from(key),
                    size: entry.size,
                });
            }
        }
    }

    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Entry;
    use crate::storage::FixtureSource;
    use std::collections::BTreeMap;
    use std::fs;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cairn-plan-{tag}-{}-{}",
            std::process::id(),
            crate::util::now_unix()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest_with(entries: &[(&str, u64, Option<&str>)]) -> Manifest {
        let mut m = Manifest::new(Path::new("/games/d2r"));
        m.complete = true;
        let mut map = BTreeMap::new();
        for (path, size, ckey) in entries {
            map.insert(
                (*path).to_string(),
                Entry {
                    size: *size,
                    ckey: ckey.map(str::to_string),
                },
            );
        }
        m.entries = map;
        m
    }

    /// Writes the file a manifest entry describes, at the right size.
    fn place(dest: &Path, rel: &str, bytes: usize) {
        let path = dest.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![b'x'; bytes]).unwrap();
    }

    fn source(dir: &Path, index: &[(&str, u64, Option<&str>, bool)]) -> FixtureSource {
        FixtureSource::new(dir.join("store"), index, None)
    }

    #[test]
    fn nothing_extracted_yet_means_everything_is_new() {
        let dir = scratch("fresh");
        let src = source(
            &dir,
            &[
                ("data:data/global/excel/a.txt", 10, Some("aa"), true),
                ("data:data/hd/b.dcc", 20, Some("bb"), true),
            ],
        );
        let plan = build(&src, &dir, None, PlanOptions::default()).unwrap();
        assert_eq!(plan.write_files(), 2);
        assert_eq!(plan.write_bytes, 30);
        assert!(plan.is_fresh());
        assert!(plan.deletes.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn identical_keys_and_present_files_write_nothing() {
        let dir = scratch("noop");
        let src = source(
            &dir,
            &[("data:data/global/excel/a.txt", 10, Some("aa"), true)],
        );
        place(&dir, "data/global/excel/a.txt", 10);
        let m = manifest_with(&[("data/global/excel/a.txt", 10, Some("aa"))]);
        let plan = build(&src, &dir, Some(&m), PlanOptions::default()).unwrap();
        assert_eq!(plan.write_files(), 0);
        assert_eq!(plan.unchanged, 1);
        assert!(plan.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_changed_content_key_rewrites_the_file() {
        let dir = scratch("changed");
        let src = source(
            &dir,
            &[("data:data/global/excel/a.txt", 10, Some("NEW"), true)],
        );
        place(&dir, "data/global/excel/a.txt", 10);
        let m = manifest_with(&[("data/global/excel/a.txt", 10, Some("OLD"))]);
        let plan = build(&src, &dir, Some(&m), PlanOptions::default()).unwrap();
        assert_eq!(plan.writes.len(), 1);
        assert_eq!(plan.writes[0].reason, WriteReason::ContentChanged);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_new_size_rewrites_the_file() {
        let dir = scratch("size");
        let src = source(
            &dir,
            &[("data:data/global/excel/a.txt", 11, Some("aa"), true)],
        );
        place(&dir, "data/global/excel/a.txt", 10);
        let m = manifest_with(&[("data/global/excel/a.txt", 10, Some("aa"))]);
        let plan = build(&src, &dir, Some(&m), PlanOptions::default()).unwrap();
        assert_eq!(plan.writes[0].reason, WriteReason::SizeChanged);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_deleted_file_is_rewritten_even_though_its_key_matches() {
        let dir = scratch("missing");
        let src = source(
            &dir,
            &[("data:data/global/excel/a.txt", 10, Some("aa"), true)],
        );
        let m = manifest_with(&[("data/global/excel/a.txt", 10, Some("aa"))]);
        let plan = build(&src, &dir, Some(&m), PlanOptions::default()).unwrap();
        assert_eq!(plan.writes[0].reason, WriteReason::Missing);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_the_archives_dropped_is_a_delete() {
        let dir = scratch("delete");
        let src = source(
            &dir,
            &[("data:data/global/excel/kept.txt", 4, Some("k"), true)],
        );
        place(&dir, "data/global/excel/kept.txt", 4);
        place(&dir, "data/global/excel/gone.txt", 9);
        let m = manifest_with(&[
            ("data/global/excel/kept.txt", 4, Some("k")),
            ("data/global/excel/gone.txt", 9, Some("g")),
        ]);
        let plan = build(&src, &dir, Some(&m), PlanOptions::default()).unwrap();
        assert_eq!(plan.writes.len(), 0);
        assert_eq!(plan.deletes.len(), 1);
        assert_eq!(
            plan.deletes[0].rel_path,
            PathBuf::from("data/global/excel/gone.txt")
        );
        assert_eq!(plan.deletes[0].size, 9);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn verify_catches_a_file_edited_outside_the_app() {
        let dir = scratch("damaged");
        // The manifest names the md5 of ten 'x' bytes.
        let good = crate::verify::hash_bytes(&[b'x'; 10]);
        let src = source(
            &dir,
            &[("data:data/global/excel/a.txt", 10, Some(&good), true)],
        );
        // On disk: ten 'y' bytes, same length, so only a hash can tell.
        fs::create_dir_all(dir.join("data/global/excel")).unwrap();
        fs::write(dir.join("data/global/excel/a.txt"), vec![b'y'; 10]).unwrap();
        let m = manifest_with(&[("data/global/excel/a.txt", 10, Some(&good))]);

        let trusting = build(&src, &dir, Some(&m), PlanOptions::default()).unwrap();
        assert_eq!(
            trusting.write_files(),
            0,
            "size and key agree, so nothing is noticed"
        );

        let checking = build(
            &src,
            &dir,
            Some(&m),
            PlanOptions {
                verify_contents: true,
            },
        )
        .unwrap();
        assert_eq!(checking.writes.len(), 1);
        assert_eq!(checking.writes[0].reason, WriteReason::Damaged);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_cdn_only_entry_is_still_planned() {
        // It exists in the index; the extraction attempt is what reports the failure.
        let dir = scratch("cdn");
        let src = source(&dir, &[("data:data/global/x.bin", 7, Some("cc"), false)]);
        let plan = build(&src, &dir, None, PlanOptions::default()).unwrap();
        assert_eq!(plan.writes.len(), 1);
        assert!(!plan.writes[0].entry.available);
        fs::remove_dir_all(&dir).ok();
    }
}
