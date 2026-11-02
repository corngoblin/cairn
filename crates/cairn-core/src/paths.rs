//! How a CASC virtual path becomes a file on disk.
//!
//! This is the one piece of D2R-specific knowledge in the program, and getting it
//! wrong means the game finds nothing. The rules, all inherited from what D2R
//! itself expects in `-direct` mode and from what D2R File Extractor established
//! works in practice:
//!
//! * A virtual path looks like `data:data/global/excel/x.txt`. Everything up to and
//!   including the first `:` is the TACT namespace, and is discarded.
//! * What remains is the path the game asks for, relative to the installation
//!   folder: `data/global/excel/x.txt`.
//! * Localised files arrive as `data:locales/audio/itit/data/local/lng/...` and
//!   `data:locales/data/itit/data/local/lng/...`. The first three segments after
//!   the namespace (`locales/<kind>/<language>`) are packaging, not layout, and are
//!   discarded — which is why a localised file lands exactly on top of the English
//!   one it replaces.
//!
//! Only three trees are extracted: `data/global`, `data/hd` and `data/local`. That
//! is the whole of the game's data, and it is what makes an extraction ~150k files
//! rather than the ~500k an everything-in-the-archive copy would be.

use std::path::{Component, Path, PathBuf};

/// The trees an extraction covers. Case-insensitive match, after the namespace.
pub const BASE_PREFIXES: &[&str] = &["data:data/global/", "data:data/hd/", "data:data/local/"];

/// Language codes D2R ships localised data for, as they appear in the archive.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("deDE", "German"),
    ("enUS", "English (US)"),
    ("esES", "Spanish (Spain)"),
    ("esMX", "Spanish (Latin America)"),
    ("frFR", "French"),
    ("itIT", "Italian"),
    ("jaJP", "Japanese"),
    ("koKR", "Korean"),
    ("plPL", "Polish"),
    ("ptBR", "Portuguese (Brazil)"),
    ("ruRU", "Russian"),
    ("zhCN", "Chinese (Simplified)"),
    ("zhTW", "Chinese (Traditional)"),
];

/// The prefixes a run should cover, given whether localised files were asked for.
pub fn prefixes(international: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = BASE_PREFIXES.iter().map(|s| s.to_string()).collect();
    if let Some(lang) = international {
        let lc = lang.to_ascii_lowercase();
        out.push(format!("data:locales/audio/{lc}/"));
        out.push(format!("data:locales/data/{lc}/"));
    }
    out
}

/// Normalises a virtual path to `/` separators so prefix tests behave the same way
/// whether CascLib handed us Windows-style or Unix-style separators.
pub fn normalize_virtual(path: &str) -> String {
    path.replace('\\', "/")
}

/// Everything after the TACT namespace: `data:x/y` → `x/y`.
pub fn strip_namespace(path: &str) -> &str {
    match path.find(':') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Whether the path (already normalised) sits in a `locales/...` tree.
pub fn is_locale_path(normalized: &str) -> bool {
    strip_namespace(normalized)
        .to_ascii_lowercase()
        .starts_with("locales/")
}

/// Whether a virtual path is one this program extracts and writes.
pub fn matches_prefixes(path: &str, prefixes: &[String]) -> bool {
    let normalized = normalize_virtual(path).to_ascii_lowercase();
    prefixes
        .iter()
        .any(|p| normalized.starts_with(&p.to_ascii_lowercase()))
}

/// The path to write, relative to the destination, for a virtual path.
///
/// Returns `None` for anything that is not a safe relative path — an absolute
/// path, a `..` component, or an empty result. Nothing in a well-formed D2R
/// archive triggers that; it exists so that a malformed or hostile archive cannot
/// make the program write outside the folder the user picked.
pub fn output_rel_path(virtual_path: &str) -> Option<PathBuf> {
    let normalized = normalize_virtual(virtual_path);
    let mut rest = strip_namespace(&normalized);

    if rest.to_ascii_lowercase().starts_with("locales/") {
        // Skip `locales`, the kind, and the language code.
        let mut consumed = 0;
        let mut idx = 0;
        for (i, c) in rest.char_indices() {
            if c == '/' {
                consumed += 1;
                if consumed == 3 {
                    idx = i + 1;
                    break;
                }
            }
        }
        if idx == 0 {
            return None; // malformed locale path, nothing sensible to write
        }
        rest = &rest[idx..];
    }

    if rest.is_empty() {
        return None;
    }

    let path = Path::new(rest);
    if path.is_absolute() {
        return None;
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            // `..`, `.`, a root, or a drive prefix: refuse. A `..` in an archive
            // path is the classic archive-extraction attack.
            _ => return None,
        }
    }
    Some(path.to_path_buf())
}

/// Whether `path` is the same folder as, or inside, `parent`. Both are expected
/// to be absolute and normalised.
///
/// The separator in the comparison is what makes this correct rather than nearly
/// correct: `/games/d2r-mods` is *not* inside `/games/d2r`.
pub fn is_under(path: &Path, parent: &Path) -> bool {
    path != parent && path.starts_with(parent)
}

/// Whether two destinations would tread on each other.
///
/// Nesting has to be refused, not merely discouraged: an update deletes files
/// under its own tree that the archives no longer contain, so two nested
/// destinations would delete each other's extraction.
pub fn destinations_overlap(a: &Path, b: &Path) -> bool {
    a == b || is_under(a, b) || is_under(b, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_tree_maps_under_data() {
        assert_eq!(
            output_rel_path("data:data/global/excel/experience.txt").unwrap(),
            PathBuf::from("data/global/excel/experience.txt")
        );
    }

    #[test]
    fn hd_and_local_trees_map_through() {
        assert_eq!(
            output_rel_path("data:data/hd/characters/hd_hero.dcc").unwrap(),
            PathBuf::from("data/hd/characters/hd_hero.dcc")
        );
        assert_eq!(
            output_rel_path("data:data/local/lng/strings/game.json").unwrap(),
            PathBuf::from("data/local/lng/strings/game.json")
        );
    }

    #[test]
    fn windows_separators_are_normalised() {
        assert_eq!(
            output_rel_path(r"data:data\global\excel\itemtypes.txt").unwrap(),
            PathBuf::from("data/global/excel/itemtypes.txt")
        );
    }

    #[test]
    fn localised_data_lands_on_top_of_the_english_path() {
        assert_eq!(
            output_rel_path("data:locales/data/itit/data/local/lng/strings/game.json").unwrap(),
            PathBuf::from("data/local/lng/strings/game.json")
        );
        assert_eq!(
            output_rel_path("data:locales/audio/deDE/data/hd/audio/speech/x.flac").unwrap(),
            PathBuf::from("data/hd/audio/speech/x.flac")
        );
    }

    #[test]
    fn traversal_is_refused() {
        assert!(output_rel_path("data:data/../../../etc/passwd").is_none());
        assert!(output_rel_path("data:/etc/passwd").is_none());
        assert!(output_rel_path("data:").is_none());
    }

    #[test]
    fn prefix_matching_is_case_insensitive_and_namespace_aware() {
        let p = prefixes(None);
        assert!(matches_prefixes("data:data/global/x.txt", &p));
        assert!(matches_prefixes(r"data:Data\Global\x.txt", &p));
        assert!(!matches_prefixes("data:data/other/x.txt", &p));
        assert!(!matches_prefixes("data:locales/data/itit/x.txt", &p));
    }

    #[test]
    fn language_prefixes_are_added() {
        let p = prefixes(Some("itIT"));
        assert!(matches_prefixes(
            "data:locales/data/itit/data/local/x.txt",
            &p
        ));
        assert!(matches_prefixes(
            "data:locales/audio/itit/data/hd/x.flac",
            &p
        ));
        assert!(!matches_prefixes(
            "data:locales/data/dede/data/local/x.txt",
            &p
        ));
    }

    #[test]
    fn nesting_is_detected_but_a_shared_prefix_is_not() {
        let a = Path::new("/games/d2r");
        let b = Path::new("/games/d2r-mods");
        assert!(!destinations_overlap(a, b));
        assert!(destinations_overlap(a, &a.join("extract")));
    }
}
