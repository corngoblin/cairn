//! Check-summing extracted files against the keys recorded for them.
//!
//! Off by default, because it reads the entire extraction — minutes on 45 GB — and
//! the ordinary update path never needs it. It is the only way to catch a file
//! that was damaged, truncated or edited outside the app: size and content key in
//! the manifest both still describe the file as it was extracted, so nothing
//! cheaper notices.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use md5::{Digest, Md5};

use crate::manifest::Manifest;
use crate::progress::{Phase, Progress, ProgressSink};

/// MD5 of a byte slice, lowercase hex. This is the same fingerprint CASC keeps as
/// a content key, which is why it can be compared against one.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// MD5 of a file, lowercase hex. `None` when it cannot be read.
pub fn hash_file(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(format!("{:x}", hasher.finalize()))
}

#[derive(Debug, Default)]
pub struct VerifyReport {
    pub checked: u64,
    pub ok: u64,
    /// Recorded, on disk, but the bytes are not what was extracted.
    pub mismatched: Vec<PathBuf>,
    /// Recorded, but not on disk any more.
    pub missing: Vec<PathBuf>,
    /// Entries with no recorded key, which cannot be checked.
    pub unverifiable: u64,
    pub cancelled: bool,
}

impl VerifyReport {
    pub fn is_clean(&self) -> bool {
        self.mismatched.is_empty() && self.missing.is_empty()
    }
}

/// Hashes every recorded file in `destination` and compares it with the manifest.
pub fn verify(
    destination: &Path,
    manifest: &Manifest,
    progress: &ProgressSink<'_>,
    cancel: &AtomicBool,
) -> VerifyReport {
    let total = manifest.entries.len() as u64;
    let mut report = VerifyReport::default();

    for (index, (key, entry)) in manifest.entries.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            return report;
        }
        let Some(expected) = entry.ckey.as_deref() else {
            report.unverifiable += 1;
            continue;
        };

        let path = destination.join(key);
        if !path.exists() {
            report.missing.push(PathBuf::from(key));
            report.checked += 1;
            continue;
        }
        match hash_file(&path) {
            Some(actual) if actual == expected => report.ok += 1,
            Some(_) => report.mismatched.push(PathBuf::from(key)),
            None => report.mismatched.push(PathBuf::from(key)),
        }
        report.checked += 1;

        if index % 200 == 0 {
            progress.report(&Progress {
                phase: Phase::Verifying,
                files_done: report.checked,
                files_total: total,
                bytes_done: report.checked,
                bytes_total: total,
                current: key.clone(),
            });
        }
    }

    progress.report(&Progress {
        phase: Phase::Verifying,
        files_done: report.checked,
        files_total: total,
        bytes_done: report.checked,
        bytes_total: total,
        current: String::new(),
    });
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Entry;
    use std::fs;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cairn-verify-{tag}-{}-{}",
            std::process::id(),
            crate::util::now_unix()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn md5_matches_a_known_digest() {
        // md5sum of the empty file, and of "abc".
        assert_eq!(hash_bytes(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hash_bytes(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn a_file_and_the_same_bytes_hash_the_same() {
        let dir = scratch("file");
        let path = dir.join("x.bin");
        fs::write(&path, b"hello cairn").unwrap();
        assert_eq!(hash_file(&path), Some(hash_bytes(b"hello cairn")));
        assert_eq!(hash_file(&dir.join("absent.bin")), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn verify_names_what_is_wrong_and_ignores_what_is_right() {
        let dir = scratch("report");
        let mut m = Manifest::new(Path::new("/games/d2r"));
        let good = hash_bytes(b"good");
        fs::create_dir_all(dir.join("data")).unwrap();
        fs::write(dir.join("data/good.txt"), b"good").unwrap();
        fs::write(dir.join("data/bad.txt"), b"BGAD").unwrap(); // same length, other bytes
        for (key, ckey) in [
            ("data/good.txt", Some(good.clone())),
            ("data/bad.txt", Some(hash_bytes(b"bad"))),
            ("data/absent.txt", Some(hash_bytes(b"gone"))),
            ("data/nokey.txt", None),
        ] {
            m.entries.insert(key.to_string(), Entry { size: 4, ckey });
        }

        let cancel = AtomicBool::new(false);
        let report = verify(&dir, &m, &ProgressSink::silent(), &cancel);
        assert_eq!(report.ok, 1);
        assert_eq!(report.mismatched, vec![PathBuf::from("data/bad.txt")]);
        assert_eq!(report.missing, vec![PathBuf::from("data/absent.txt")]);
        assert_eq!(report.unverifiable, 1);
        assert!(!report.is_clean());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancellation_stops_the_pass() {
        let dir = scratch("cancel");
        let mut m = Manifest::new(Path::new("/games/d2r"));
        m.entries.insert(
            "a".into(),
            Entry {
                size: 1,
                ckey: Some(hash_bytes(b"a")),
            },
        );
        let cancel = AtomicBool::new(true);
        let report = verify(&dir, &m, &ProgressSink::silent(), &cancel);
        assert!(report.cancelled);
        fs::remove_dir_all(&dir).ok();
    }
}
