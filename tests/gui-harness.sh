#!/bin/sh
# Drive the Cairn window under Xvfb and capture what it actually draws.
#
# Everything happens in one session so the app and the tools share a bus and a
# display. The config is seeded first, because the file dialog cannot be driven
# synthetically — and the config layout is what the app writes anyway.
set -eu

BIN=${1:-./target/debug/cairn-gui}
HOME_DIR=/root/cairn-test/home
CONFIG_DIR=/root/cairn-test/config
SHOTS=/root/cairn-test/shots

rm -rf "$HOME_DIR" "$CONFIG_DIR" "$SHOTS"
mkdir -p "$HOME_DIR/games/Diablo II Resurrected" "$CONFIG_DIR/cairn" "$SHOTS"

# A folder that passes the "is this a D2R installation" check but whose archives
# are not real: exactly what a user pointing at the wrong folder sees, and it
# exercises the error path end to end.
printf 'Branch!STRING:0\nBuild Key!HEX:16\n' > "$HOME_DIR/games/Diablo II Resurrected/.build.info"

cat > "$CONFIG_DIR/cairn/config.json" <<EOF
{
  "version": 1,
  "installations": [
    {
      "path": "$HOME_DIR/games/Diablo II Resurrected",
      "destinations": [
        { "path": "$HOME_DIR/games/Diablo II Resurrected", "enabled": true }
      ]
    }
  ],
  "verify_contents": false
}
EOF

chmod 700 /root/cairn-test 2>/dev/null || true
export HOME=$HOME_DIR
export XDG_CONFIG_HOME=$CONFIG_DIR
export XDG_CACHE_HOME=/root/cairn-test/cache
export XDG_DATA_HOME=/root/cairn-test/data
export LANG=C.UTF-8
export GSK_RENDERER=cairo
export XDG_RUNTIME_DIR=/root/cairn-test/run
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CACHE_HOME" "$XDG_DATA_HOME"
chmod 700 "$XDG_RUNTIME_DIR"

xvfb-run -a --server-args="-screen 0 1400x900x24" dbus-run-session -- sh -c "
  $BIN > /root/cairn-test/app.log 2>&1 &
  APP=\$!
  sleep 5
  import -window root $SHOTS/01-list.png

  # The window must be focused before a click counts.
  WID=\$(xdotool search --onlyvisible --name 'Cairn' | head -1)
  xdotool windowactivate --sync \$WID 2>/dev/null || true
  sleep 1

  # The row's Extract button sits at the right end of the first list row.
  xdotool mousemove 688 95 click 1
  sleep 4
  import -window root $SHOTS/02-extract.png

  # The row menu (three dots) is to the right of that button.
  xdotool mousemove 743 95 click 1
  sleep 2
  import -window root $SHOTS/03-menu.png

  # "Destinations..." is the first item in that popover.
  xdotool mousemove 729 148 click 1
  sleep 2
  import -window root $SHOTS/04-destinations.png

  # Close the dialog, then open the app menu and its Settings entry.
  xdotool key Escape
  sleep 1
  xdotool mousemove 703 27 click 1
  sleep 1
  import -window root $SHOTS/05-appmenu.png
  xdotool mousemove 703 78 click 1
  sleep 2
  import -window root $SHOTS/06-settings.png

  kill \$APP 2>/dev/null || true
  sleep 1
"

echo "== shots:"
ls -la "$SHOTS"
echo "== log (first 60 lines):"
head -60 /root/cairn-test/app.log 2>/dev/null || true
