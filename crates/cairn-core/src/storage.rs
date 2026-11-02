//! Where the archives are read from, and what an extraction can see in them.
//!
//! The rest of the program does not care that CASC is involved: it asks a
//! [`Source`] what files exist and for the bytes of one of them. Two
//! implementations exist — the real CASC storage, and a fixture directory used by
//! the tests — so that every piece of logic above this line (path mapping,
//! planning, manifests, undo) is exercised without a 45 GB game install.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::paths;
use crate::progress::{Phase, Progress, ProgressSink};

/// One file the archives contain and the extraction covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    /// The path inside the storage, e.g. `data:data/global/excel/x.txt`.
    pub virtual_path: String,
    /// Where it belongs, relative to a destination.
    pub rel_path: PathBuf,
    pub size: u64,
    /// Lowercase hex content key, when the storage knows one.
    pub ckey: Option<String>,
    /// False for an entry that exists in the index but not on this disk — a
    /// CDN-only file. Such a file enumerates and then fails to read.
    pub available: bool,
}

impl SourceEntry {
    /// The manifest key for this entry: its relative path, `/`-separated.
    pub fn key(&self) -> String {
        self.rel_path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// A readable set of game archives.
pub trait Source {
    /// Every covered file, sorted by relative path, with localised files already
    /// resolved over the English ones they replace.
    fn entries(&self) -> &[SourceEntry];

    /// Streams one file's decoded bytes into `out`. `Ok(None)` means the entry
    /// could not be opened.
    fn copy_to(&self, virtual_path: &str, out: &mut dyn Write) -> io::Result<Option<u64>>;

    /// The directory the archives themselves live in, recorded in the manifest so
    /// a later update knows what it was extracted from.
    fn root_path(&self) -> &Path;

    /// A short human description of the storage, for logs and the UI.
    fn describe(&self) -> String;
}

// ---------------------------------------------------------------------------
// CASC
// ---------------------------------------------------------------------------

/// A real D2R installation's CASC storage.
pub struct CascSource {
    storage: casc_sys::Storage,
    entries: Vec<SourceEntry>,
    language: Option<String>,
}

impl CascSource {
    /// Opens an installation and enumerates every file the extraction covers.
    ///
    /// This is the slow part of a first run — CascLib parses the index, the
    /// encoding manifest and the ROOT file before the first entry appears — so it
    /// reports progress and honours cancellation.
    pub fn open(
        installation: &Path,
        language: Option<&str>,
        progress: &ProgressSink<'_>,
        cancel: &AtomicBool,
    ) -> Result<Self, SourceError> {
        progress.report(&Progress::enumerating("opening game archives…"));
        let storage = casc_sys::Storage::open(installation).map_err(|e| SourceError::Open {
            path: installation.to_path_buf(),
            source: e,
        })?;

        let prefixes = paths::prefixes(language);
        progress.report(&Progress::enumerating("reading file index…"));

        // Keyed by output path: a localised file deliberately lands on top of the
        // English one it replaces, and writing both would make the result depend on
        // archive enumeration order.
        let mut by_output: BTreeMap<PathBuf, SourceEntry> = BTreeMap::new();
        let mut seen = 0u64;
        let mut overridden = 0u64;

        let find = storage
            .find("*")
            .map_err(|e| SourceError::Enumerate(e.to_string()))?;
        for data in find {
            seen += 1;
            if seen % 20_000 == 0 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(SourceError::Cancelled);
                }
                progress.report(&Progress {
                    phase: Phase::Enumerating,
                    current: format!("scanning — {seen} archive entries"),
                    ..Progress::default()
                });
            }

            // Entries CascLib can only name by key or file data ID have no path the
            // game would ever ask for.
            if !data.has_real_name() {
                continue;
            }
            let virtual_path = match data.path() {
                Some(p) => p,
                None => continue,
            };
            if !paths::matches_prefixes(&virtual_path, &prefixes) {
                continue;
            }
            let rel_path = match paths::output_rel_path(&virtual_path) {
                Some(p) => p,
                None => continue,
            };

            let is_locale = paths::is_locale_path(&virtual_path);
            if let Some(existing) = by_output.get(&rel_path) {
                // The selected language wins over the base file; otherwise the
                // first one seen keeps the slot.
                if !is_locale || paths::is_locale_path(&existing.virtual_path) {
                    continue;
                }
                overridden += 1;
            }

            let ckey = {
                let hex = data.ckey_hex();
                if hex == "00000000000000000000000000000000" {
                    None
                } else {
                    Some(hex)
                }
            };
            by_output.insert(
                rel_path.clone(),
                SourceEntry {
                    virtual_path,
                    rel_path,
                    size: data.file_size,
                    ckey,
                    available: data.is_available_locally(),
                },
            );
        }

        if by_output.is_empty() {
            return Err(SourceError::Empty);
        }

        progress.report(&Progress {
            phase: Phase::Enumerating,
            current: format!(
                "{} files found{}",
                by_output.len(),
                if overridden > 0 {
                    format!(" ({overridden} replaced by the selected language)")
                } else {
                    String::new()
                }
            ),
            ..Progress::default()
        });

        Ok(Self {
            storage,
            entries: by_output.into_values().collect(),
            language: language.map(str::to_string),
        })
    }

    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }
}

impl Source for CascSource {
    fn entries(&self) -> &[SourceEntry] {
        &self.entries
    }

    fn copy_to(&self, virtual_path: &str, out: &mut dyn Write) -> io::Result<Option<u64>> {
        self.storage.copy_file(virtual_path, out)
    }

    fn describe(&self) -> String {
        format!("CASC storage at {}", self.storage.opened_at().display())
    }

    fn root_path(&self) -> &Path {
        self.storage.opened_at()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("could not open game archives at {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not enumerate the archives: {0}")]
    Enumerate(String),
    #[error("no D2R data files were found in this storage")]
    Empty,
    #[error("cancelled")]
    Cancelled,
}

// ---------------------------------------------------------------------------
// Fixture source (tests)
// ---------------------------------------------------------------------------

/// A fake storage backed by a directory of files plus a JSON index.
///
/// The index uses the storage's own virtual-path form — including the TACT
/// namespace and the localised trees — so the code under test sees exactly the
/// paths a real archive produces, and the fixture can express things a real
/// download cannot: a CDN-only entry, a file that vanished, a locale override.
pub struct FixtureSource {
    pub entries: Vec<SourceEntry>,
    root: PathBuf,
}

impl FixtureSource {
    /// `index` is a list of `(virtual_path, size, ckey, available)`.
    /// Sizes and keys are taken from the index rather than the files on disk, so a
    /// fixture can describe content that is deliberately absent.
    pub fn new(
        root: impl Into<PathBuf>,
        index: &[(&str, u64, Option<&str>, bool)],
        language: Option<&str>,
    ) -> Self {
        let root = root.into();
        let prefixes = paths::prefixes(language);
        let mut by_output: BTreeMap<PathBuf, SourceEntry> = BTreeMap::new();
        for (virtual_path, size, ckey, available) in index {
            if !paths::matches_prefixes(virtual_path, &prefixes) {
                continue;
            }
            let Some(rel_path) = paths::output_rel_path(virtual_path) else {
                continue;
            };
            let is_locale = paths::is_locale_path(virtual_path);
            if let Some(existing) = by_output.get(&rel_path) {
                if !is_locale || paths::is_locale_path(&existing.virtual_path) {
                    continue;
                }
            }
            by_output.insert(
                rel_path.clone(),
                SourceEntry {
                    virtual_path: (*virtual_path).to_string(),
                    rel_path,
                    size: *size,
                    ckey: ckey.map(str::to_string),
                    available: *available,
                },
            );
        }
        Self {
            entries: by_output.into_values().collect(),
            root,
        }
    }

    /// Adds a file's bytes to the fixture, in the layout a real read would need.
    /// The name on disk is the virtual path, flattened into a directory tree.
    pub fn put(&self, virtual_path: &str, bytes: &[u8]) -> io::Result<()> {
        let flat = virtual_path.replace(':', "__").replace(['/', '\\'], "__");
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(self.root.join(flat), bytes)
    }
}

impl Source for FixtureSource {
    fn entries(&self) -> &[SourceEntry] {
        &self.entries
    }

    fn copy_to(&self, virtual_path: &str, out: &mut dyn Write) -> io::Result<Option<u64>> {
        let flat = virtual_path.replace(':', "__").replace(['/', '\\'], "__");
        let path = self.root.join(flat);
        match std::fs::read(&path) {
            Ok(bytes) => {
                out.write_all(&bytes)?;
                Ok(Some(bytes.len() as u64))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn root_path(&self) -> &Path {
        &self.root
    }

    fn describe(&self) -> String {
        format!("fixture storage at {}", self.root.display())
    }
}
