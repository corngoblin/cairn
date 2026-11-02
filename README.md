# Cairn

**Extract Diablo II: Resurrected's data files on Linux, so the game loads from disk
instead of decompressing its archives every time a zone loads.**

Cairn unpacks D2R's art, sound and data into plain files, keeps a record of every
file it wrote, updates only what a game patch changed, and can undo the whole thing
without touching anything you put in the same folder.

It is a Linux-native replacement for
[D2R File Extractor](https://github.com/levinium/D2RExtractor), which does the same
job on Windows. The feature set is deliberately the same — that project has already
worked out what matters (manifest, incremental update, undo, several destinations,
localised files), and this is a faithful port of it rather than a re-imagining.
This was 1000% made with the help of AI, if that bothers you don't use it and move on. 

<img width="849" height="631" alt="Screenshot From 2026-11-02 00-12-00" src="https://github.com/user-attachments/assets/141ef7f7-30bf-4871-a270-f020a2ab9b9a" />

---

## Why this exists

D2R reads its assets out of compressed CASC archives. Every zone load decompresses
what it needs. Unpack the archives to plain files and pass `-direct -txt` to the
game, and it reads instead of decompressing — which is where most of the loading
time goes.

On Windows there is a tool for that. On Linux the game runs through Proton or Steam,
the archives are the same, and there was nothing to unwrap them with.

## What it does

- **Reads both storage formats.** Battle.net installs use classic CASC (`.build.info`,
  index files, ENCODING manifest, BLTE-compressed blobs). A Steam install since build
  93236 uses a static container (`data/.build.config`, 40-bit file offsets, no
  encoding manifest at all). Both are read through
  [CascLib](https://github.com/ladislav-zezula/CascLib) — the engine behind Ladik's
  CASC Viewer — which supports both, on Linux, compiled from source in this repository.
- **Records everything it writes.** Every extracted file is stored in a manifest with
  the content key the archive itself keeps for that content. That record is what makes
  the next three features possible.
- **Updates instead of starting over.** After a patch, Cairn compares the archives'
  content keys against the recorded ones and rewrites only the files that actually
  changed. It never reads the 45 GB back to find out.
- **Undo removes only what Cairn created.** Files you put in the folder yourself are
  not in the manifest, so they are not touched. Folders the undo empties are pruned.
- **Several destinations.** The game folder, plus a mods folder, plus a staging copy —
  each with its own records, extracted, updated and undone independently.
- **Localised files.** D2R ships dubbed audio and localised text for 13 languages.
  Pick one and it replaces the English files rather than adding to them, which is what
  the game expects.
- **Finds installations.** Steam libraries named in `libraryfolders.vdf`, Proton
  prefixes under `compatdata`, Wine and Bottles prefixes, and a copy sitting in your
  home directory.

## Using it

1. Install the flatpak (see below) and launch **Cairn**.
2. Press **Extract** next to your installation. Battle.net installs take roughly
   30–45 minutes; how long depends on your disk.
3. Add `-direct -txt` to the game's launch options:

   - **Steam:** right-click the game → Properties → Launch Options → `-direct -txt`
   - **Battle.net / Proton:** add the same flags to the shortcut's arguments

That third step is the one everyone forgets. Without it the game ignores the files
entirely and nothing changes.

Each installation needs about **40–45 GB** of free space for the extracted files.

## Installing

Two formats, same program. The flatpak is the tidier one on a GNOME desktop; the
AppImage needs nothing installed and works anywhere with a reasonably recent glibc.

```sh
# flatpak, from a release bundle
flatpak install --user io.github.corngoblin.Cairn.flatpak

# or without a runtime: one file, no install
chmod +x Cairn-x86_64.AppImage && ./Cairn-x86_64.AppImage
```

Building the flatpak needs `flatpak`, `org.flatpak.Builder` and the GNOME 50 SDK — no
root, nothing else on the host. Building the AppImage needs GTK 4.14 / libadwaita 1.5
headers and `squashfs-tools`, and is meant to run on the *oldest* base you intend to
support, because the binary links the glibc it is built against (it is built on
Ubuntu 24.04 for that reason). `./appimage/build.sh` also verifies, before packing,
that a symbolic icon really decodes out of the bundle — that check exists because of
a bug where every icon silently became a placeholder.

## The command line

The same engine, without a window. Useful for scripting, and for diagnosing a
machine over SSH:

```sh
cairn scan --add                    # find installations and remember them
cairn status                        # what is extracted, where
cairn inspect ~/games/d2r           # open the archives and describe them
cairn extract ~/games/d2r --dry-run # what an extract would write
cairn extract ~/games/d2r --to ~/mods --lang itIT
cairn verify ~/games/d2r            # hash every extracted file against the record
cairn changes ~/games/d2r           # what the last run added, replaced, removed
cairn undo ~/games/d2r              # delete exactly what was extracted
```

`cairn` is installed beside the window inside the flatpak, so
`flatpak run --command=cairn io.github.corngoblin.Cairn status` works too.

## Where things live

| What | Where |
| --- | --- |
| Extracted game data | `<destination>/data/global`, `data/hd`, `data/local` |
| Cairn's record of a destination | `<destination>/.cairn/manifest.json` |
| Last run's change list | `<destination>/.cairn/last-run.json` |
| Settings | `~/.var/app/io.github.corngoblin.Cairn/config/cairn/config.json` in the flatpak, `~/.config/cairn/config.json` otherwise |

The manifest lives **inside** the folder it describes rather than in the app's own
data directory. That way it survives the app being uninstalled, the folder being
moved, and the settings being thrown away — the three ways a record kept elsewhere is
lost while 45 GB of extracted files stay behind.

## How it is put together

```
casc-sys            CascLib FFI, built from vendor/CascLib at compile time
cairn-core          paths, planning, manifest, engine, detection — all of the logic
cairn-cli           the `cairn` command
cairn-app           the GTK4 / libadwaita window
appimage/           build.sh, a pixbuf probe, and the clean-container check
vendor/CascLib      Ladislav Zezula's CASC reader, vendored (see its LICENSE and PATCHES.md)
flatpak/            build.sh, and the generated offline crate list
```

The flatpak manifest is `io.github.corngoblin.Cairn.json` at the repository root:
a `dir` source may not point outside the manifest's own directory, so the manifest
has to sit at the top of the tree it packages.

Two decisions worth knowing about:

**The plan is separate from the work.** An extraction is computed first, as a list of
writes and deletes, entirely from the archive index and the manifest. That is why the
preview (`--dry-run`) is truthful, why "already up to date" is instant, and why the
logic that decides *what to delete* can be tested without a game install.

**Everything above the archive reader is testable without archives.**
`cairn-core`'s tests drive the real engine against a
[`FixtureSource`](crates/cairn-core/src/storage.rs) — archive-shaped virtual paths
backed by files in a temporary directory — so extraction, incremental update, locale
overrides, interruption and undo are all exercised in `cargo test`, including the
cases that are expensive to get wrong (a nested destination, a file the user added,
a `..` in an archive path).

## Tests

```sh
cargo test --workspace
```

The suite runs without a display or a game install. It covers, among other things:
path mapping from CASC virtual paths, traversal refusal, destination overlap,
manifest atomicity and format versioning, the diff reasons (new / missing / size /
content / damaged), a full extract → patch → update → undo round trip, locale
override, interruption, and the `CASC_FIND_DATA` layout against the C compiler's.

## Credits

- [CascLib](https://github.com/ladislav-zezula/CascLib) by Ladislav Zezula — all of
  the archive reading. Vendored under `vendor/CascLib`, MIT licensed.
- [D2R File Extractor](https://github.com/levinium/D2RExtractor) by levinium — the
  feature set, the manifest design and the destination model are ports of that
  project's, which is the reason this could be written in an afternoon instead of a
  month.

Cairn is not affiliated with Blizzard Entertainment. Diablo II: Resurrected is theirs.
This program only reads files you already have on your disk.

## License

MIT — see [LICENSE](LICENSE).
