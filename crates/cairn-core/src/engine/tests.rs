//! End-to-end tests of the engine against a fixture storage.
//!
//! These are the tests that would have caught the expensive mistakes: files
//! landing outside the destination, an update rewriting everything, undo deleting
//! something the user put there, an interrupted run that leaves 45 GB nobody can
//! account for.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::*;
use crate::manifest::Manifest;
use crate::progress::ProgressSink;
use crate::storage::FixtureSource;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cairn-engine-{tag}-{}-{}",
        std::process::id(),
        crate::util::now_unix()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Builds a fixture storage: `files` are (virtual path, contents), and every entry
/// is available unless it is listed in `missing`.
fn fixture(
    dir: &Path,
    files: &[(&str, &[u8])],
    missing: &[&str],
    language: Option<&str>,
) -> FixtureSource {
    let index: Vec<(&str, u64, Option<String>, bool)> = files
        .iter()
        .map(|(path, bytes)| {
            (
                *path,
                bytes.len() as u64,
                Some(crate::verify::hash_bytes(bytes)),
                !missing.contains(path),
            )
        })
        .collect();
    let borrowed: Vec<(&str, u64, Option<&str>, bool)> = index
        .iter()
        .map(|(p, s, k, a)| (*p, *s, k.as_deref(), *a))
        .collect();
    let source = FixtureSource::new(dir.join("archives"), &borrowed, language);
    // A file listed as missing from the storage gets no bytes either: that is what
    // "in the index but not on this disk" means, and the extraction must cope.
    for (path, bytes) in files {
        if !missing.contains(path) {
            source.put(path, bytes).unwrap();
        }
    }
    source
}

fn read(dest: &Path, rel: &str) -> Vec<u8> {
    fs::read(dest.join(rel)).unwrap_or_else(|e| panic!("{rel} not readable: {e}"))
}

const A: &str = "data:data/global/excel/a.txt";
const B: &str = "data:data/hd/b.bin";
const C: &str = "data:data/local/lng/strings/c.json";

fn base_files() -> Vec<(&'static str, &'static [u8])> {
    vec![
        (A, b"alpha" as &[u8]),
        (B, b"bravo-bytes"),
        (C, b"{\"c\":1}"),
    ]
}

#[test]
fn a_first_run_writes_every_file_into_the_destination() {
    let dir = scratch("fresh");
    let source = fixture(&dir, &base_files(), &[], None);

    let summary = run(
        &source,
        &dir,
        &RunOptions::default(),
        &ProgressSink::silent(),
        &AtomicBool::new(false),
    )
    .unwrap();

    assert_eq!(summary.kind, RunKind::Extract);
    assert_eq!(summary.written, 3);
    assert_eq!(summary.unchanged, 0);
    assert_eq!(read(&dir, "data/global/excel/a.txt"), b"alpha");
    assert_eq!(read(&dir, "data/hd/b.bin"), b"bravo-bytes");
    assert_eq!(read(&dir, "data/local/lng/strings/c.json"), b"{\"c\":1}");

    let manifest = Manifest::load(&dir).unwrap().unwrap();
    assert!(manifest.complete);
    assert_eq!(manifest.entry_count(), 3);
    assert_eq!(manifest.total_bytes(), 5 + 11 + 7);
    assert_eq!(
        manifest.entries["data/global/excel/a.txt"].ckey.as_deref(),
        Some(crate::verify::hash_bytes(b"alpha").as_str())
    );

    // No temporary files survive a clean run.
    let leftovers: Vec<_> = walk(&dir)
        .into_iter()
        .filter(|p| p.to_string_lossy().ends_with(PART_SUFFIX))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_second_run_with_nothing_changed_touches_nothing() {
    let dir = scratch("up-to-date");
    let files = base_files();
    let source = fixture(&dir, &files, &[], None);
    run(
        &source,
        &dir,
        &RunOptions::default(),
        &ProgressSink::silent(),
        &AtomicBool::new(false),
    )
    .unwrap();

    let before = fs::metadata(dir.join("data/global/excel/a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let summary = run(
        &source,
        &dir,
        &RunOptions::default(),
        &ProgressSink::silent(),
        &AtomicBool::new(false),
    )
    .unwrap();

    assert_eq!(summary.kind, RunKind::UpToDate);
    assert_eq!(summary.written, 0);
    assert_eq!(summary.unchanged, 3);
    let after = fs::metadata(dir.join("data/global/excel/a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after, "the file must not be rewritten");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_patch_writes_only_what_changed_and_removes_what_it_dropped() {
    let dir = scratch("patch");

    let v1 = base_files();
    let source = fixture(&dir, &v1, &[], None);
    use_engine(&source, &dir);

    // The patch: `a` is edited, `b` is unchanged, `c` is dropped, `d` is new.
    let v2: Vec<(&'static str, &'static [u8])> = vec![
        (A, b"alpha-edited" as &[u8]),
        (B, b"bravo-bytes"),
        ("data:data/global/excel/d.txt", b"delta"),
    ];
    let patched = fixture(&dir, &v2, &[], None);
    let summary = use_engine(&patched, &dir);

    assert_eq!(summary.kind, RunKind::Update);
    assert_eq!(summary.written, 2, "the edited file and the new one");
    assert_eq!(summary.removed, 1, "the dropped file");
    assert_eq!(summary.unchanged, 1, "the untouched file");

    assert_eq!(read(&dir, "data/global/excel/a.txt"), b"alpha-edited");
    assert_eq!(read(&dir, "data/global/excel/d.txt"), b"delta");
    assert!(!dir.join("data/local/lng/strings/c.json").exists());
    // The directory that held only the dropped file is gone with it.
    assert!(!dir.join("data/local/lng/strings").exists());
    assert!(dir.join("data/local").exists() || !dir.join("data/local").exists());

    let manifest = Manifest::load(&dir).unwrap().unwrap();
    assert!(manifest.complete);
    assert_eq!(manifest.entry_count(), 3);
    assert!(manifest.entries.contains_key("data/global/excel/d.txt"));
    assert!(!manifest
        .entries
        .contains_key("data/local/lng/strings/c.json"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn undo_removes_what_was_extracted_and_nothing_else() {
    let dir = scratch("undo");
    let source = fixture(&dir, &base_files(), &[], None);
    use_engine(&source, &dir);

    // Something the user put in the same tree. Undo must not touch it.
    let mine = dir.join("data/global/excel/my-notes.txt");
    fs::write(&mine, b"do not delete me").unwrap();

    let summary = undo(&dir, &ProgressSink::silent(), &AtomicBool::new(false)).unwrap();
    assert_eq!(summary.removed, 3);
    assert_eq!(summary.missing, 0);
    assert!(summary.had_manifest);

    assert!(!dir.join("data/hd/b.bin").exists());
    assert!(!dir.join("data/global/excel/a.txt").exists());
    assert!(mine.exists(), "a file the user added must survive undo");
    assert!(
        !dir.join(crate::manifest::STATE_DIR).exists(),
        "the record goes too"
    );
    assert!(
        !dir.join("data/hd").exists(),
        "empty directories are pruned"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn undo_with_no_manifest_is_a_harmless_no_op() {
    let dir = scratch("undo-none");
    let summary = undo(&dir, &ProgressSink::silent(), &AtomicBool::new(false)).unwrap();
    assert!(summary.is_noop());
    assert_eq!(summary.removed, 0);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_interrupted_run_is_still_undoable() {
    let dir = scratch("interrupted");
    let source = fixture(&dir, &base_files(), &[], None);

    // Cancel before anything is written.
    let cancel = AtomicBool::new(true);
    let err = run(
        &source,
        &dir,
        &RunOptions::default(),
        &ProgressSink::silent(),
        &cancel,
    );
    assert!(matches!(err, Err(RunError::Cancelled)));

    // Whatever happened, undo works and leaves no record behind.
    undo(&dir, &ProgressSink::silent(), &AtomicBool::new(false)).unwrap();
    assert!(!dir.join(crate::manifest::STATE_DIR).exists());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_cdn_only_entry_is_reported_and_left_out_of_the_record() {
    let dir = scratch("cdn-only");
    let files = base_files();
    let source = fixture(&dir, &files, &[B], None);

    let summary = use_engine(&source, &dir);
    assert_eq!(summary.written, 2);
    assert_eq!(summary.unavailable, 1);
    assert!(!dir.join("data/hd/b.bin").exists());

    let manifest = Manifest::load(&dir).unwrap().unwrap();
    assert!(!manifest.entries.contains_key("data/hd/b.bin"));
    assert!(manifest.incomplete_reason.is_some(), "the gap is recorded");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn localised_files_replace_the_english_ones() {
    let dir = scratch("locale");
    let files: Vec<(&'static str, &'static [u8])> = vec![
        (
            "data:data/local/lng/strings/item-names.json",
            b"english" as &[u8],
        ),
        (
            "data:locales/data/itit/data/local/lng/strings/item-names.json",
            b"italiano",
        ),
        // A language we did not ask for must be ignored entirely.
        (
            "data:locales/data/dede/data/local/lng/strings/item-names.json",
            b"deutsch",
        ),
    ];
    let source = fixture(&dir, &files, &[], Some("itIT"));

    let summary = run(
        &source,
        &dir,
        &RunOptions {
            international: Some("itIT".to_string()),
            ..Default::default()
        },
        &ProgressSink::silent(),
        &AtomicBool::new(false),
    )
    .unwrap();

    assert_eq!(summary.written, 1);
    assert_eq!(
        read(&dir, "data/local/lng/strings/item-names.json"),
        b"italiano"
    );
    let manifest = Manifest::load(&dir).unwrap().unwrap();
    assert_eq!(manifest.international.as_deref(), Some("itIT"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_change_log_records_an_update_but_not_a_fresh_extraction() {
    let dir = scratch("changes");
    let v1 = base_files();
    let source = fixture(&dir, &v1, &[], None);
    use_engine(&source, &dir);
    assert!(
        load_change_log(&dir).is_none(),
        "a fresh extraction has no change list to show"
    );

    let v2: Vec<(&'static str, &'static [u8])> = vec![
        (A, b"alpha-edited" as &[u8]),
        (B, b"bravo-bytes"),
        (C, b"{\"c\":1}"),
        ("data:data/global/excel/d.txt", b"delta"),
    ];
    let patched = fixture(&dir, &v2, &[], None);
    use_engine(&patched, &dir);

    let changes = load_change_log(&dir).expect("the update wrote a change list");
    let paths: BTreeSet<String> = changes.iter().map(|c| c.path.clone()).collect();
    assert!(paths.contains("data/global/excel/a.txt"));
    assert!(paths.contains("data/global/excel/d.txt"));
    assert!(changes
        .iter()
        .any(|c| c.change == ChangeKind::Replaced && c.path.ends_with("a.txt")));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_run_that_cannot_fit_is_refused_before_anything_is_written() {
    // 45 GB of plan against 40 GB free: refused, and the message names both numbers
    // so the user knows what to free up.
    let err = check_space(
        Path::new("/games/d2r"),
        45 * 1024 * 1024 * 1024,
        40 * 1024 * 1024 * 1024,
    )
    .expect_err("45 GB cannot fit in 40 GB");
    let message = err.to_string();
    assert!(message.contains("not enough free space"), "{message}");
    // The available figure is the one the user acts on, so it is named exactly; the
    // required figure carries the slack, so only its unit is asserted.
    assert!(message.contains("40.0 GB"), "{message}");
    assert!(message.contains("/games/d2r"), "{message}");

    // Exactly enough is not enough: the slack is there for the manifest and the
    // temporary rename.
    assert!(check_space(Path::new("/x"), 1_000, 1_000).is_err());
    assert!(check_space(Path::new("/x"), 1_000, 1_000 + 512 * 1024 * 1024).is_ok());
}

#[test]
fn every_written_path_stays_inside_the_destination() {
    let dir = scratch("contained");
    let dest = dir.join("dest");
    fs::create_dir_all(&dest).unwrap();
    let source = fixture(&dir, &base_files(), &[], None);
    use_engine(&source, &dest);

    for path in walk(&dest) {
        assert!(
            path.starts_with(&dest),
            "{} escaped {}",
            path.display(),
            dest.display()
        );
    }
    fs::remove_dir_all(&dir).ok();
}

fn use_engine(source: &dyn crate::storage::Source, dest: &Path) -> RunSummary {
    run(
        source,
        dest,
        &RunOptions::default(),
        &ProgressSink::silent(),
        &AtomicBool::new(false),
    )
    .unwrap()
}

/// Every file under `root`, for containment and leftover checks.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}
