//! Finding D2R installations without making the user hunt through a Proton prefix.
//!
//! On Linux, "where is your D2R folder" is a genuinely awkward question. There are
//! at least four shapes of answer:
//!
//! * a Steam library at `<lib>/steamapps/common/Diablo II Resurrected`
//! * a Steam install running through Proton, whose real files are inside
//!   `<lib>/steamapps/compatdata/<id>/pfx/drive_c/Program Files (x86)/…`
//! * a Battle.net install inside a Proton prefix the same way
//! * a game copied somewhere by hand, out of reach of any launcher
//!
//! The first three are discoverable, so they are discovered: Steam's own
//! `libraryfolders.vdf` names every library it knows about, including ones on other
//! drives, and every `pfx` under every `compatdata` directory is checked for the
//! game. The fourth cannot be found, which is why "add a folder" is not a
//! fallback buried in a menu but the obvious other button.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The name Blizzard's installer uses, in every one of those layouts.
const GAME_DIR_NAME: &str = "Diablo II Resurrected";

/// Files that prove a folder really is a D2R installation, in the order they are
/// worth checking: `.build.info` for Battle.net/classic CASC, `data/.build.config`
/// for a Steam static storage (build 93236+).
const MARKERS: &[&str] = &[
    ".build.info",
    ".build.db",
    "versions",
    "data/.build.config",
    "Data/.build.config",
];

/// Whether `path` looks like a D2R installation.
pub fn is_installation(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    MARKERS.iter().any(|m| path.join(m).exists())
}

/// Every D2R installation findable on this machine, best guess first.
///
/// `home` is passed in rather than read from the environment so the search can be
/// tested against a fixture tree.
pub fn find_installations(home: &Path) -> Vec<PathBuf> {
    // Each root with its own depth budget: a Steam library buries the game seven
    // levels down inside a Proton prefix, while a loose copy in the home directory
    // is two levels down and the home directory itself is too big to walk deeply.
    let mut roots: Vec<(PathBuf, usize)> = Vec::new();

    // Steam: the canonical install, plus whatever libraryfolders.vdf adds. A
    // library on another drive is only reachable this way.
    for steam in [home.join(".steam/steam"), home.join(".local/share/Steam")] {
        if steam.is_dir() {
            for library in steam_libraries(&steam) {
                roots.push((library, 8));
            }
            roots.push((steam, 8));
        }
    }

    // Wine and Bottles prefix roots, for a Battle.net install outside Steam.
    for wine in [
        home.join(".wine/drive_c"),
        home.join(".local/share/bottles/bottles"),
    ] {
        if wine.is_dir() {
            roots.push((wine, 8));
        }
    }
    for games in [home.join("Games"), home.join("Drive_C")] {
        if games.is_dir() {
            roots.push((games, 6));
        }
    }
    // A copy sitting straight in the home directory, or one folder below it.
    if home.is_dir() {
        roots.push((home.to_path_buf(), 3));
    }

    let mut found = BTreeSet::new();
    for (root, depth) in roots {
        for hit in search(&root, 0, depth) {
            if is_installation(&hit) {
                found.insert(hit);
            }
        }
    }
    found.into_iter().collect()
}

/// Steam library roots named in a `libraryfolders.vdf`, plus the usual defaults.
///
/// The file is Valve KeyValues text: `"path"  "/mnt/games/SteamLibrary"`. A real
/// parser is not needed for one field, but the quotes and the escaped backslashes
/// are, so the line is read rather than regexed.
fn steam_libraries(steam: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut candidates = vec![
        steam.join("steamapps/libraryfolders.vdf"),
        steam.join("config/libraryfolders.vdf"),
    ];
    if let Ok(real) = std::fs::canonicalize(steam) {
        candidates.push(real.join("steamapps/libraryfolders.vdf"));
    }

    for vdf in candidates {
        let Ok(text) = std::fs::read_to_string(&vdf) else {
            continue;
        };
        for line in text.lines() {
            let trimmed = line.trim();
            if !trimmed.to_ascii_lowercase().starts_with("\"path\"") {
                continue;
            }
            if let Some(value) = trimmed.split('"').nth(3) {
                let path = PathBuf::from(value.replace("\\\\", "/"));
                if path.is_dir() {
                    out.push(path);
                }
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

/// Depth-limited search for a directory called `Diablo II Resurrected`, or for a
/// directory that directly contains a D2R marker file.
fn search(root: &Path, depth: usize, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if depth > max_depth {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // A symlinked library can loop; do not follow one back into itself.
        if kind.is_symlink() {
            continue;
        }
        // Dot-directories are handled explicitly (`.steam`, `.wine`, Bottles); walking
        // every cache and config directory in a home folder is pure cost.
        if name.starts_with('.') {
            continue;
        }
        if name == GAME_DIR_NAME {
            out.push(path);
            continue;
        }
        // Proton prefixes bury the game five levels down; nothing else here goes
        // anywhere near eight.
        out.extend(search(&path, depth + 1, max_depth));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cairn-detect-{tag}-{}-{}",
            std::process::id(),
            crate::util::now_unix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_battle_net_install_is_recognised() {
        let dir = scratch("bn");
        let game = dir.join("Diablo II Resurrected");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join(".build.info"), b"Branch!STRING:0").unwrap();
        assert!(is_installation(&game));
        assert_eq!(find_installations(&dir), vec![game.clone()]);

        // And an empty folder of the right name is not.
        let empty = dir.join("SteamLibrary/steamapps/common");
        std::fs::create_dir_all(empty.join("Diablo II Resurrected")).unwrap();
        assert!(!is_installation(&empty.join("Diablo II Resurrected")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_proton_prefix_install_is_found_through_a_steam_library() {
        let dir = scratch("proton");
        // A Steam library on another drive, named by the user's Steam install.
        let library = dir.join("SteamLibrary");
        let game = library.join(
            "steamapps/compatdata/2344520/pfx/drive_c/Program Files (x86)/Diablo II Resurrected",
        );
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join(".build.info"), b"x").unwrap();

        let steam = dir.join(".steam/steam");
        std::fs::create_dir_all(steam.join("steamapps")).unwrap();
        std::fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                library.display()
            ),
        )
        .unwrap();

        let found = find_installations(&dir);
        assert!(found.contains(&game), "{found:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_loose_copy_one_folder_below_home_is_found() {
        let dir = scratch("loose");
        let game = dir.join("games/Diablo II Resurrected");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join(".build.info"), b"x").unwrap();
        assert_eq!(find_installations(&dir), vec![game.clone()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_steam_static_storage_is_recognised_by_its_own_marker() {
        let dir = scratch("static");
        let game = dir.join("Diablo II Resurrected");
        std::fs::create_dir_all(game.join("Data")).unwrap();
        std::fs::write(game.join("Data/.build.config"), b"{}").unwrap();
        assert!(is_installation(&game));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn library_folders_are_read_from_the_vdf() {
        let dir = scratch("vdf");
        let lib = dir.join("games/SteamLibrary");
        std::fs::create_dir_all(lib.join("steamapps")).unwrap();
        let steam = dir.join(".steam/steam");
        std::fs::create_dir_all(steam.join("steamapps")).unwrap();
        std::fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                lib.display()
            ),
        )
        .unwrap();

        let game = lib.join("steamapps/common/Diablo II Resurrected");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join(".build.info"), b"x").unwrap();

        let found = find_installations(&dir);
        assert!(found.contains(&game), "{found:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
