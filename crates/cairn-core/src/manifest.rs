//! The record of what an extraction put on disk.
//!
//! Three features depend on this file, and all three are impossible without it:
//!
//! * **Undo** removes *exactly* the files Cairn wrote, and leaves anything the user
//!   put in the same folder alone.
//! * **Update** notices which files a patch changed without reading 45 GB back,
//!   because the content key each entry stores is the fingerprint the archive
//!   itself keeps for that content.
//! * **Resume** is possible at all after a crash, because the manifest is written —
//!   marked incomplete — *before* the first file is written, so an interrupted run
//!   still knows what it touched.
//!
//! It lives at `<destination>/.cairn/manifest.json`, inside the tree it describes.
//! Keeping it there rather than in the app's own data directory means it survives
//! the app being uninstalled, the folder being moved, and the user clearing their
//! config — the three ways a record kept elsewhere is lost while 45 GB of files
//! stay behind.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::util::{self, now_unix};

/// Directory name inside a destination that holds Cairn's own bookkeeping.
pub const STATE_DIR: &str = ".cairn";
/// Manifest file name inside [`STATE_DIR`].
pub const MANIFEST_FILE: &str = "manifest.json";
/// The manifest format this build writes and understands.
pub const CURRENT_VERSION: u32 = 1;

/// Where a content key came from, so a later update knows whether stored keys are
/// comparable with freshly enumerated ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum KeySource {
    /// CascLib supplied the content key (MD5 of the decoded content).
    #[default]
    CascCkey,
    /// No keys available; updates fall back to size comparisons.
    None,
}

/// One extracted file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Byte size as recorded by the archive at extraction time.
    pub size: u64,
    /// Lowercase hex content key, when the storage supplied one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ckey: Option<String>,
}

/// What an extraction wrote, and what it wrote it from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    /// Unix seconds.
    pub created_unix: i64,
    pub updated_unix: i64,
    /// False while a run is in progress, or after one was interrupted. An
    /// incomplete manifest still names every file written so far, which is what
    /// makes it safe to undo or resume.
    pub complete: bool,
    /// The installation whose archives these files came from.
    pub storage_path: PathBuf,
    /// Language code whose localised files were applied, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub international: Option<String>,
    /// `path → entry`, relative to the destination, `/`-separated.
    pub entries: BTreeMap<String, Entry>,
    #[serde(default)]
    pub key_source: KeySource,
    /// Set when a run finished with files it could not read. Those are absent from
    /// `entries`, so a later run retries them.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub incomplete_reason: Option<String>,
}

impl Manifest {
    /// A fresh, empty manifest for a destination, already marked incomplete.
    pub fn new(storage_path: &Path) -> Self {
        let now = now_unix();
        Self {
            version: CURRENT_VERSION,
            created_unix: now,
            updated_unix: now,
            complete: false,
            storage_path: storage_path.to_path_buf(),
            international: None,
            entries: BTreeMap::new(),
            key_source: KeySource::CascCkey,
            incomplete_reason: None,
        }
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn total_bytes(&self) -> u64 {
        self.entries.values().map(|e| e.size).sum()
    }

    /// Path of the manifest inside a destination.
    pub fn path_in(destination: &Path) -> PathBuf {
        destination.join(STATE_DIR).join(MANIFEST_FILE)
    }

    /// Reads the manifest for a destination. `Ok(None)` when there is none, which
    /// is the normal state before a first extraction.
    pub fn load(destination: &Path) -> Result<Option<Self>, ManifestError> {
        let path = Self::path_in(destination);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ManifestError::Io(path, e)),
        };
        let manifest: Manifest =
            serde_json::from_str(&text).map_err(|e| ManifestError::Parse(path.clone(), e))?;
        if manifest.version > CURRENT_VERSION {
            return Err(ManifestError::Newer(manifest.version));
        }
        Ok(Some(manifest))
    }

    /// Writes the manifest atomically: a reader (or a crash) sees either the whole
    /// previous file or the whole new one, never a half-written record of 150k
    /// files.
    pub fn save(&mut self, destination: &Path) -> Result<(), ManifestError> {
        self.updated_unix = now_unix();
        let path = Self::path_in(destination);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| ManifestError::Io(parent.into(), e))?;
        }
        let json = serde_json::to_vec(self).map_err(|e| ManifestError::Parse(path.clone(), e))?;

        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &json).map_err(|e| ManifestError::Io(tmp.clone(), e))?;
        // fsync before the rename, so a power cut cannot leave an empty file where
        // the manifest was.
        if let Ok(file) = fs::File::open(&tmp) {
            let _ = file.sync_all();
        }
        fs::rename(&tmp, &path).map_err(|e| ManifestError::Io(path, e))?;
        Ok(())
    }

    /// Deletes the manifest and its state directory. Used by undo once the files
    /// are gone.
    pub fn remove(destination: &Path) -> io::Result<()> {
        let dir = destination.join(STATE_DIR);
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("could not read or write {0}: {1}")]
    Io(PathBuf, io::Error),
    #[error("manifest {0} is not valid JSON: {1}")]
    Parse(PathBuf, serde_json::Error),
    #[error("manifest was written by a newer Cairn (format {0}); this build understands {CURRENT_VERSION}")]
    Newer(u32),
}

/// A one-line description of the manifest's state, for a UI row or a CLI listing.
pub fn describe(destination: &Path) -> String {
    match Manifest::load(destination) {
        Ok(None) => "not extracted".to_string(),
        Ok(Some(m)) if m.complete => format!(
            "{} files, {} (updated {})",
            m.entry_count(),
            util::format_bytes(m.total_bytes()),
            util::format_unix(m.updated_unix)
        ),
        Ok(Some(m)) => format!(
            "interrupted — {} files on record ({})",
            m.entry_count(),
            util::format_unix(m.updated_unix)
        ),
        Err(e) => format!("unreadable manifest: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cairn-manifest-{tag}-{}-{}",
            std::process::id(),
            now_unix()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tmpdir("roundtrip");
        let mut m = Manifest::new(Path::new("/games/d2r"));
        m.entries.insert(
            "data/global/excel/x.txt".into(),
            Entry {
                size: 42,
                ckey: Some("abcdef".into()),
            },
        );
        m.complete = true;
        m.save(&dir).unwrap();

        let loaded = Manifest::load(&dir).unwrap().unwrap();
        assert_eq!(loaded.entry_count(), 1);
        assert_eq!(loaded.total_bytes(), 42);
        assert!(loaded.complete);
        assert_eq!(
            loaded.entries["data/global/excel/x.txt"].ckey.as_deref(),
            Some("abcdef")
        );
        assert!(Manifest::path_in(&dir).exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_manifest_is_not_an_error() {
        let dir = tmpdir("missing");
        assert!(Manifest::load(&dir).unwrap().is_none());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_leaves_no_temporary_behind() {
        let dir = tmpdir("tmpfile");
        let mut m = Manifest::new(Path::new("/games/d2r"));
        m.save(&dir).unwrap();
        let leftovers: Vec<_> = fs::read_dir(dir.join(STATE_DIR))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["manifest.json".to_string()]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_future_format_is_refused_rather_than_misread() {
        let dir = tmpdir("future");
        fs::create_dir_all(dir.join(STATE_DIR)).unwrap();
        fs::write(
            Manifest::path_in(&dir),
            r#"{"version":99,"created_unix":0,"updated_unix":0,"complete":true,"storage_path":"/x","entries":{}}"#,
        )
        .unwrap();
        assert!(matches!(
            Manifest::load(&dir),
            Err(ManifestError::Newer(99))
        ));
        fs::remove_dir_all(&dir).ok();
    }
}
