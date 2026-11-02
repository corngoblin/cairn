#!/bin/sh
# Prove the AppImage draws its window using only what it carries.
#
# Run inside a container with NO GTK packages: xvfb and a screenshot tool, and
# nothing the bundle could lean on. If the window appears correctly, everything
# libadwaita needs — libraries, GSettings schemas, the icon theme, the pixbuf
# loaders and their own dependencies — is inside the image.
#
# Two traps this script exists to avoid:
#   * `pkill -f cairn-gui` matches the shell running this script, because the pattern
#     appears in its own command line. It kills the harness instead of the app, and
#     the screenshot that remains is the previous run's. Use `pkill -x`.
#   * the app has no exit path, so anything waiting on it hangs. Kill by pid.
set -eu

APPIMAGE=${1:-/p/cairn/Cairn-x86_64.AppImage}
SHOT=${2:-/root/appimage-shot.png}
LOG=/root/appimage.log

rm -rf "$HOME/.cache/cairn" "$SHOT" "$LOG"

echo "== gtk packages installed here: $(dpkg -l 2>/dev/null | grep -cE 'libgtk-4|libadwaita')"

xvfb-run -a --server-args="-screen 0 1400x900x24" sh -c "
    $APPIMAGE --appimage-extract-and-run > $LOG 2>&1 &
    APP=\$!
    sleep 9
    import -window root $SHOT 2>/dev/null || true
    kill \$APP 2>/dev/null || true
    sleep 1
    pkill -x cairn-gui 2>/dev/null || true
"

echo "== startup line"
grep -m1 "ready" "$LOG" || echo "(no startup line — the window never got built)"
echo "== pixbuf cache"
CACHE="$HOME/.cache/cairn/loaders.cache"
if [ -f "$CACHE" ]; then
    echo "   $(grep -c '^\"' "$CACHE") loaders, svg entries: $(grep -c 'pixbufloader-svg' "$CACHE")"
    FIRST=$(grep -o '/[^\"]*\.so' "$CACHE" | head -1)
    echo "   first path on disk: $(test -f "$FIRST" && echo yes || echo NO)"
else
    echo "   (no cache written)"
fi
echo "== warnings from the app"
grep -iE "warning|error|critical" "$LOG" | grep -v "Unable to acquire session bus" | head -5 || true
echo "== screenshot: $(ls -la "$SHOT" 2>/dev/null | awk '{print $5" bytes"}')"
