#!/bin/sh
# Prove the INSTALLED flatpak on this machine's real GNOME session.
#
# A bundle that installs is not a bundle that works: missing icon themes, a schema
# the runtime does not carry, or a library the runtime does not have all show up only
# when the packaged binary actually starts. This launches it against the live
# session (Wayland), screenshots the desktop through the shell, and closes it again.
#
#   ./tests/flatpak-session-shot.sh [output.png]
set -eu

APP_ID=io.github.corngoblin.Cairn
OUT=${1:-/home/ferah/cairn-sandbox.png}
UID_NUM=$(id -u)

export XDG_RUNTIME_DIR=/run/user/$UID_NUM
export WAYLAND_DISPLAY=wayland-0
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$UID_NUM/bus
export XDG_SESSION_TYPE=wayland
export XDG_CURRENT_DESKTOP=GNOME

echo "== version reported by the installed bundle"
flatpak run --command=cairn-gui "$APP_ID" --version

echo "== libraries the packaged binary resolves against the runtime"
flatpak run --command=sh "$APP_ID" -c 'ldd /app/bin/cairn-gui | grep -c "not found" || true'

echo "== launching"
flatpak run "$APP_ID" > /tmp/cairn-sandbox.log 2>&1 &
APP=$!
sleep 7

if flatpak ps --columns=application 2>/dev/null | grep -q "$APP_ID"; then
    echo "== running: yes"
else
    echo "== running: NO (it exited)"
fi

# The shell's own screenshot interface. Non-interactive, no approval dialog.
gdbus call --session --dest org.gnome.Shell.Screenshot \
    --object-path /org/gnome/Shell/Screenshot \
    --method org.gnome.Shell.Screenshot.Screenshot false false "$OUT" 2>&1 | head -2 || true

sleep 1
kill "$APP" 2>/dev/null || true
flatpak kill "$APP_ID" 2>/dev/null || true

echo "== app log"
tail -20 /tmp/cairn-sandbox.log
ls -la "$OUT" 2>/dev/null || echo "(no screenshot written)"
