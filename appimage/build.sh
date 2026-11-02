#!/bin/sh
# Build a self-contained AppImage of Cairn.
#
#   ./appimage/build.sh            build Cairn-x86_64.AppImage
#
# Unlike the flatpak, which borrows GTK from the GNOME runtime, an AppImage has to
# carry everything: the libraries, the GSettings schemas libadwaita reads, the icon
# theme its symbolic icons come from, and the gdk-pixbuf loaders for any image the
# toolkit decodes. Miss one and the app starts on the machine it was built on and
# aborts on the user's with a message about a schema or a missing loader.
#
# Run this on the OLDEST base you intend to support. The binary links against the
# glibc it is built with, and glibc is the one library that must not be bundled
# (a private glibc breaks the dynamic loader), so the base image *is* the
# compatibility floor. That is why this is built on Ubuntu 24.04 and the flatpak on
# GNOME 50: the AppImage needs the wider reach.
set -eu

cd "$(dirname "$0")/.."
ROOT=$(pwd)
APP_ID=io.github.corngoblin.Cairn
ARCH=$(uname -m)
BUILD=$ROOT/.appimage-build
APPDIR=$BUILD/Cairn.AppDir
OUT=$ROOT/Cairn-$ARCH.AppImage

echo "== building release binaries"
cargo build --release --locked -p cairn-app -p cairn-cli

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" "$APPDIR/usr/share"

install -m755 target/release/cairn-gui "$APPDIR/usr/bin/cairn-gui"
install -m755 target/release/cairn "$APPDIR/usr/bin/cairn"

# --- libraries -------------------------------------------------------------
# Everything the two binaries link against, minus the C runtime itself. libc,
# libm, libpthread, libdl, librt and the dynamic loader are the host's business;
# carrying them is how an AppImage ends up unusable on any machine but its own.
echo "== collecting libraries"
{
    ldd target/release/cairn-gui
    ldd target/release/cairn
} | awk '{for (i = 1; i <= NF; i++) if ($i ~ /^\//) print $i}' | sort -u |
    grep -v -E '/(libc|libm|libpthread|libdl|librt|libresolv|libnsl|libutil|libanl)\.so|/ld-linux' > "$BUILD/libs.txt" || true

while read -r lib; do
    [ -f "$lib" ] || continue
    cp -L "$lib" "$APPDIR/usr/lib/"
done < "$BUILD/libs.txt"
echo "   $(ls "$APPDIR/usr/lib" | wc -l) libraries"

# --- GSettings schemas -----------------------------------------------------
# libadwaita and GTK both read org.gtk.gtk4.Settings.*, and a missing schema is a
# hard abort at startup, not a warning.
echo "== copying GSettings schemas"
mkdir -p "$APPDIR/usr/share/glib-2.0/schemas"
cp /usr/share/glib-2.0/schemas/*.gschema.xml "$APPDIR/usr/share/glib-2.0/schemas/" 2>/dev/null || true
glib-compile-schemas "$APPDIR/usr/share/glib-2.0/schemas"

# --- gdk-pixbuf loaders ----------------------------------------------------
# GTK rasterises every icon through gdk-pixbuf, and symbolic icons are SVG, so the
# SVG loader has to work or the whole UI renders as missing-image placeholders.
# Two things are easy to get wrong here:
#
#   * the loader's own dependencies are not linked by the app (librsvg is linked by
#     libpixbufloader-svg.so, nothing else), so `ldd` on the binaries does not see
#     them. They are collected by running ldd over the loaders as well.
#   * gdk-pixbuf-query-loaders *silently omits* any loader it cannot dlopen, so a
#     missing librsvg produces a cache with no SVG entry and no error anywhere.
#     The build therefore fails if that entry is absent.
echo "== collecting gdk-pixbuf loaders"
PIXBUF_DIR=$(dirname "$(find /usr/lib -name 'loaders.cache' -path '*gdk-pixbuf*' | head -1)")
if [ -z "$PIXBUF_DIR" ] || [ ! -d "$PIXBUF_DIR/loaders" ]; then
    echo "error: gdk-pixbuf loader directory not found" >&2
    exit 1
fi

LOADERS=$APPDIR/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders
mkdir -p "$LOADERS"
cp "$PIXBUF_DIR"/loaders/*.so "$LOADERS/"

# Dependencies of the loaders, not of the app.
for so in "$LOADERS"/*.so; do
    ldd "$so" 2>/dev/null
done | awk '{for (i = 1; i <= NF; i++) if ($i ~ /^\//) print $i}' | sort -u |
    grep -v -E '/(libc|libm|libpthread|libdl|librt|libresolv|libnsl|libutil|libanl)\.so|/ld-linux' > "$BUILD/loader-libs.txt" || true

while read -r lib; do
    [ -f "$lib" ] || continue
    base=$(basename "$lib")
    [ -f "$APPDIR/usr/lib/$base" ] && continue
    cp -L "$lib" "$APPDIR/usr/lib/"
done < "$BUILD/loader-libs.txt"

QUERY=$(command -v gdk-pixbuf-query-loaders 2>/dev/null ||
        find /usr -name 'gdk-pixbuf-query-loaders' -type f 2>/dev/null | head -1)
[ -n "$QUERY" ] || { echo "error: gdk-pixbuf-query-loaders not found" >&2; exit 1; }
install -m755 "$QUERY" "$APPDIR/usr/bin/gdk-pixbuf-query-loaders"

# Generated here, with the loader directory named explicitly, and with the AppDir
# path replaced by a placeholder: the mount point differs on every launch, so AppRun
# substitutes it at startup.
LD_LIBRARY_PATH="$APPDIR/usr/lib" GDK_PIXBUF_MODULEDIR="$LOADERS" \
    "$QUERY" > "$BUILD/loaders.cache"
if ! grep -q 'pixbufloader-svg' "$BUILD/loaders.cache"; then
    echo "error: the SVG pixbuf loader could not be loaded, so every icon would be blank." >&2
    echo "       Check that librsvg is installed and that its libraries are in usr/lib." >&2
    exit 1
fi
mkdir -p "$APPDIR/usr/share/cairn"
sed "s|$APPDIR|@APPDIR@|g" "$BUILD/loaders.cache" > "$APPDIR/usr/share/cairn/loaders.cache.in"
echo "   $(grep -c '^"' "$BUILD/loaders.cache") loaders, svg included"

# --- shared MIME database ------------------------------------------------
# gdk-pixbuf decides an image's format by content type first, and that lookup needs
# /usr/share/mime. Without it, an SVG file falls through to magic-byte sniffing,
# which the SVG loader only matches for a file starting " <svg" — Adwaita's icons
# start with an XML declaration, so every symbolic icon fails to load and the whole
# UI renders as missing-image placeholders. Text still draws, which makes it look
# like an icon theme problem rather than a MIME problem.
echo "== copying the shared MIME database"
mkdir -p "$APPDIR/usr/share/mime"
cp /usr/share/mime/mime.cache "$APPDIR/usr/share/mime/" 2>/dev/null ||
    echo "   warning: no mime.cache to copy; SVG icons may not load" >&2

# --- icons -----------------------------------------------------------------
echo "== copying icon theme"
mkdir -p "$APPDIR/usr/share/icons"
cp -r /usr/share/icons/Adwaita "$APPDIR/usr/share/icons/" 2>/dev/null || true
# Every theme directory needs an index.theme, or GTK skips it when scanning.
mkdir -p "$APPDIR/usr/share/icons/hicolor/scalable/apps"
printf '[Icon Theme]\nName=Hicolor\nDirectories=scalable/apps\n\n[scalable/apps]\nSize=64\nType=Scalable\nContext=Applications\n' \
    > "$APPDIR/usr/share/icons/hicolor/index.theme"
cp data/icons/hicolor/scalable/apps/$APP_ID.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/"
cp data/icons/hicolor/scalable/apps/$APP_ID.svg "$APPDIR/$APP_ID.svg"

# --- desktop entry + AppRun ------------------------------------------------
install -m644 data/$APP_ID.desktop "$APPDIR/$APP_ID.desktop"
# AppImage wants the icon named .DirIcon as well, and the desktop file's Icon key
# is what the launcher looks up inside the bundle.
# appimagetool requires a PNG at the top level: it generates .DirIcon itself, and
# refuses an SVG one because file managers and thumbnailers cannot be expected to
# parse it.
ICON_SVG=data/icons/hicolor/scalable/apps/$APP_ID.svg
if command -v rsvg-convert >/dev/null 2>&1; then
    rsvg-convert -w 256 -h 256 "$ICON_SVG" -o "$APPDIR/$APP_ID.png"
elif command -v convert >/dev/null 2>&1; then
    convert -background none "$ICON_SVG" -resize 256x256 "$APPDIR/$APP_ID.png"
else
    echo "error: need rsvg-convert or ImageMagick to make the PNG icon" >&2
    exit 1
fi

cat > "$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
# Everything the app needs is beside this script; nothing is installed on the host.
HERE=$(dirname "$(readlink -f "$0")")
export LD_LIBRARY_PATH="$HERE/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export GSETTINGS_SCHEMA_DIR="$HERE/usr/share/glib-2.0/schemas"
export XDG_DATA_DIRS="$HERE/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"

# The pixbuf loader cache names absolute paths that differ on every launch (the
# AppImage mounts at a new path each time), and the mounted image is read-only, so
# it is generated into a writable cache directory instead.
# The loader cache names absolute paths that change on every launch: an AppImage
# mounts at a new path each time, and the mounted image is read-only. So a template
# is shipped with @APPDIR@ in place of the mount point, and the real cache is written
# to a writable directory at startup.
TEMPLATE="$HERE/usr/share/cairn/loaders.cache.in"
if [ -f "$TEMPLATE" ]; then
    CACHEDIR=${XDG_CACHE_HOME:-$HOME/.cache}/cairn
    mkdir -p "$CACHEDIR" 2>/dev/null || CACHEDIR=${TMPDIR:-/tmp}/cairn-cache
    mkdir -p "$CACHEDIR" 2>/dev/null || true
    if sed "s|@APPDIR@|$HERE|g" "$TEMPLATE" > "$CACHEDIR/loaders.cache" 2>/dev/null; then
        export GDK_PIXBUF_MODULE_FILE="$CACHEDIR/loaders.cache"
    fi
fi

# `cairn` (the CLI) is reachable as `./Cairn.AppImage --cairn <args>`.
if [ "${1:-}" = "--cairn" ]; then
    shift
    exec "$HERE/usr/bin/cairn" "$@"
fi
exec "$HERE/usr/bin/cairn-gui" "$@"
APPRUN
chmod 755 "$APPDIR/AppRun"


# --- verify the bundle can actually decode an icon -------------------------
# the failure this guards against is silent: every icon becomes a placeholder and
# the only evidence is a warning line in the app's log.
if command -v gcc >/dev/null 2>&1 && pkg-config --exists gdk-pixbuf-2.0 2>/dev/null; then
    echo "== checking that a symbolic icon decodes from inside the bundle"
    gcc -o "$BUILD/pixbuf-probe" appimage/probe/pixbuf-probe.c \
        $(pkg-config --cflags --libs gdk-pixbuf-2.0)
    ICON=$APPDIR/usr/share/icons/Adwaita/symbolic/devices/drive-harddisk-symbolic.svg
    if LD_LIBRARY_PATH="$APPDIR/usr/lib" \
       GDK_PIXBUF_MODULE_FILE="$BUILD/loaders.cache" \
       XDG_DATA_DIRS="$APPDIR/usr/share" \
       "$BUILD/pixbuf-probe" "$ICON" | grep -q "^load .*: OK"; then
        echo "   OK: symbolic SVG decodes with the bundled loaders"
    else
        echo "error: a symbolic icon does not decode from this bundle." >&2
        LD_LIBRARY_PATH="$APPDIR/usr/lib" GDK_PIXBUF_MODULE_FILE="$BUILD/loaders.cache" \
            XDG_DATA_DIRS="$APPDIR/usr/share" "$BUILD/pixbuf-probe" "$ICON" >&2 || true
        exit 1
    fi
else
    echo "   (skipped: no gcc/gdk-pixbuf headers to build the probe with)"
fi

# --- pack ------------------------------------------------------------------
echo "== packing $OUT"
if [ ! -x "$BUILD/appimagetool" ]; then
    curl -sSfL -o "$BUILD/appimagetool.AppImage" \
        https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage
    chmod +x "$BUILD/appimagetool.AppImage"
    # FUSE is usually absent in a container, so the tool extracts itself instead.
    (cd "$BUILD" && ./appimagetool.AppImage --appimage-extract >/dev/null)
    cp -r "$BUILD/squashfs-root" "$BUILD/appimagetool"
fi

ARCH=$ARCH "$BUILD/appimagetool/AppRun" --no-appstream "$APPDIR" "$OUT"
ls -la "$OUT"
sha256sum "$OUT"
