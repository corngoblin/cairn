# Changelog

## 0.1.0 — 2026-11-02

First release.

- Extract D2R's data trees (`data/global`, `data/hd`, `data/local`) from CASC storage
  into plain files, through CascLib compiled from source.
- Read both storage formats: Battle.net's classic CASC and Steam's static container
  (`data/.build.config`, build 93236+).
- Per-destination manifest recording every written file and its content key, stored in
  `<destination>/.cairn/manifest.json`.
- Update that rewrites only what the archives say changed, without re-reading the
  extraction.
- Undo that removes exactly the recorded files, prunes emptied folders, and leaves
  anything the user added alone.
- Several destinations per installation, with overlap refusal.
- Optional localised files for 13 languages, replacing the English ones in place.
- Optional content verification (MD5 against the recorded keys).
- Last run's change list, per destination.
- Installation discovery: Steam libraries from `libraryfolders.vdf`, Proton
  `compatdata` prefixes, Wine/Bottles prefixes, loose copies in the home directory.
- `cairn` command with `scan`, `status`, `inspect`, `extract`, `verify`, `changes` and
  `undo`.
- Flatpak packaging (`io.github.corngoblin.Cairn`) with an offline crate source list
  generated from `Cargo.lock`.
