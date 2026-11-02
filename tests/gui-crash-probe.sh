#!/bin/sh
# Reproduce the "click Extract, window disappears" path under gdb.
set -eu

BIN=${1:-./target/debug/cairn-gui}
HOME_DIR=/root/cairn-test/home
CONFIG_DIR=/root/cairn-test/config
SHOTS=/root/cairn-test/shots

export HOME=$HOME_DIR
export XDG_CONFIG_HOME=$CONFIG_DIR
export XDG_CACHE_HOME=/root/cairn-test/cache
export XDG_DATA_HOME=/root/cairn-test/data
export LANG=C.UTF-8
export GSK_RENDERER=cairo
export GTK_A11Y=none
export XDG_RUNTIME_DIR=/root/cairn-test/run

xvfb-run -a --server-args="-screen 0 1400x900x24" dbus-run-session -- sh -c "
  gdb -q -batch -ex run -ex 'bt full' --args $BIN > /root/cairn-test/gdb.log 2>&1 &
  GDB=\$!
  sleep 8
  WID=\$(xdotool search --onlyvisible --name 'Cairn' | head -1)
  echo \"window id: \$WID\" > /root/cairn-test/probe.txt
  xdotool windowactivate --sync \$WID 2>/dev/null || true
  sleep 1
  import -window root $SHOTS/00-before.png
  xdotool mousemove 688 95 click 1
  sleep 4
  import -window root $SHOTS/01-after.png
  echo \"windows now: \$(xdotool search --onlyvisible --name . | tr '\n' ' ')\" >> /root/cairn-test/probe.txt
  wait \$GDB 2>/dev/null || true
"

echo "== probe:"
cat /root/cairn-test/probe.txt
echo "== gdb:"
grep -nE "SIGSEGV|SIGABRT|^#[0-9]+ |panicked|thread '" /root/cairn-test/gdb.log | head -30
