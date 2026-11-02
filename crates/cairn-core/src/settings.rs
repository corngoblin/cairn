//! The user's settings: which installations exist, where each one extracts to, and
//! the two choices that change what an extraction contains.
//!
//! Deliberately small and deliberately plain JSON. Nothing here is precious — lose
//! `config.json` and the next run rebuilds it from the folders on disk and the
//! manifests inside them — which is the property that makes a flatpak sandbox
//! (where this file lives under `~/.var/app/…`) acceptable at all.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

pub const CURRENT_VERSION: u32 = 1;

/// One place a run writes to. The installation folder itself is the default, and
/// is the only one `-direct` mode can read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Destination {
    pub path: PathBuf,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,
}

fn default_true() -> bool {
    true
}

impl Destination {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            enabled: true,
            label: None,
        }
    }

    pub fn display_name(&self) -> String {
        match &self.label {
            Some(label) if !label.trim().is_empty() => label.clone(),
            _ => self.path.display().to_string(),
        }
    }
}

/// A game folder Cairn reads from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installation {
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,
    /// Where runs write. Empty means "just the installation folder".
    #[serde(default)]
    pub destinations: Vec<Destination>,
}

impl Installation {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            destinations: vec![Destination::new(path.clone())],
            path,
            label: None,
        }
    }

    pub fn display_name(&self) -> String {
        match &self.label {
            Some(label) if !label.trim().is_empty() => label.clone(),
            _ => self.path.display().to_string(),
        }
    }

    /// The destinations a run would actually write to.
    ///
    /// A disabled destination is left exactly as it is rather than cleaned up:
    /// turning one off is not the same as asking for its 45 GB to be deleted, and
    /// the operation that does ask is undo.
    pub fn active_destinations(&self) -> Vec<&Destination> {
        let active: Vec<&Destination> = self.destinations.iter().filter(|d| d.enabled).collect();
        if active.is_empty() {
            Vec::new()
        } else {
            active
        }
    }

    /// Adds a destination, refusing one that would overlap an existing one.
    pub fn add_destination(&mut self, path: PathBuf) -> Result<(), String> {
        let path = normalize(&path)?;
        for existing in &self.destinations {
            if paths::destinations_overlap(&path, &existing.path) {
                return Err(format!(
                    "{} overlaps {}, which is already a destination. Nested destinations would \
                     delete each other's files during an update.",
                    path.display(),
                    existing.path.display()
                ));
            }
        }
        self.destinations.push(Destination::new(path));
        Ok(())
    }

    pub fn remove_destination(&mut self, path: &Path) {
        if let Ok(path) = normalize(path) {
            self.destinations.retain(|d| d.path != path);
        }
    }
}

/// Absolute, with no trailing separator, so two spellings of one folder compare
/// equal.
pub fn normalize(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("cannot resolve {}: {e}", path.display()))?
            .join(path)
    };
    // `canonicalize` fails on a folder that does not exist yet, which is a normal
    // state for a destination the user is about to create.
    let resolved = fs::canonicalize(&absolute).unwrap_or(absolute);
    let mut cleaned = PathBuf::new();
    for component in resolved.components() {
        cleaned.push(component);
    }
    Ok(cleaned)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub installations: Vec<Installation>,
    /// Language code whose localised files replace the English ones, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub language: Option<String>,
    /// Hash files instead of trusting size and content key during an update.
    #[serde(default)]
    pub verify_contents: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            installations: Vec::new(),
            language: None,
            verify_contents: false,
        }
    }
}

impl Config {
    /// Where the config lives: `$CAIRN_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cairn`,
    /// else `~/.config/cairn`.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("CAIRN_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("cairn").join("config.json")
    }

    /// Reads the config. A missing or unreadable file is an empty config: settings
    /// are a convenience, and refusing to start over them would be wrong.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)
    }

    /// Adds an installation, or returns the index of the one already present.
    pub fn add_installation(&mut self, path: PathBuf) -> Result<usize, String> {
        let path = normalize(&path)?;
        if let Some(index) = self.installations.iter().position(|i| i.path == path) {
            return Ok(index);
        }
        self.installations.push(Installation::new(path));
        Ok(self.installations.len() - 1)
    }

    pub fn remove_installation(&mut self, path: &Path) {
        if let Ok(path) = normalize(path) {
            self.installations.retain(|i| i.path != path);
        }
    }

    /// Adds every installation found on this machine that is not already listed.
    /// Returns how many were added.
    pub fn discover(&mut self, home: &Path) -> usize {
        let mut added = 0;
        for found in crate::detect::find_installations(home) {
            if self.add_installation(found).is_ok() {
                added += 1;
            }
        }
        added
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cairn-config-{tag}-{}-{}",
            std::process::id(),
            crate::util::now_unix()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_and_defaults_survive_a_missing_field() {
        let dir = scratch("roundtrip");
        let path = dir.join("config.json");
        let mut config = Config::default();
        config.add_installation(dir.join("game")).unwrap();
        config.language = Some("itIT".into());
        config.save(&path).unwrap();

        let loaded = Config::load(&path);
        assert_eq!(loaded.installations.len(), 1);
        assert_eq!(loaded.language.as_deref(), Some("itIT"));
        assert!(!loaded.verify_contents);

        // A config written by an older build with fewer fields still loads.
        fs::write(&path, r#"{"version":1,"installations":[]}"#).unwrap();
        assert!(Config::load(&path).installations.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_config_is_not_an_error() {
        let dir = scratch("missing");
        let config = Config::load(&dir.join("nope/config.json"));
        assert!(config.installations.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn adding_the_same_installation_twice_is_idempotent() {
        let dir = scratch("dupe");
        let mut config = Config::default();
        let game = dir.join("game");
        fs::create_dir_all(&game).unwrap();
        let first = config.add_installation(game.clone()).unwrap();
        let second = config.add_installation(game).unwrap();
        assert_eq!(first, second);
        assert_eq!(config.installations.len(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_default_installation_extracts_into_itself() {
        let dir = scratch("default-dest");
        let game = dir.join("game");
        fs::create_dir_all(&game).unwrap();
        let installation = Installation::new(game.clone());
        let active = installation.active_destinations();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].path, game);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nested_destinations_are_refused() {
        let dir = scratch("nested");
        let game = dir.join("game");
        fs::create_dir_all(&game).unwrap();
        let mut installation = Installation::new(game.clone());

        let inner = game.join("extract");
        fs::create_dir_all(&inner).unwrap();
        let err = installation.add_destination(inner).unwrap_err();
        assert!(err.contains("overlaps"), "{err}");

        // A sibling that merely shares a name prefix is fine.
        let sibling = dir.join("game-mods");
        fs::create_dir_all(&sibling).unwrap();
        installation.add_destination(sibling).unwrap();
        assert_eq!(installation.destinations.len(), 2);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_disabled_destination_is_not_written_to() {
        let dir = scratch("disabled");
        let game = dir.join("game");
        fs::create_dir_all(&game).unwrap();
        let extra = dir.join("mods");
        fs::create_dir_all(&extra).unwrap();
        let mut installation = Installation::new(game);
        installation.add_destination(extra.clone()).unwrap();
        installation.destinations[1].enabled = false;

        let active = installation.active_destinations();
        assert_eq!(active.len(), 1);
        assert_ne!(active[0].path, extra);
        fs::remove_dir_all(&dir).ok();
    }
}
