#!/bin/bash
# Run a command against an isolated headless GNOME Shell (its own D-Bus
# session and Wayland socket), so the real GTK app can be tested without
# touching the desktop session.
#
#   scripts/headless-gnome.sh env FOLIO_SCREENSHOT=/tmp/shot.png ./target/release/folio README.md
#   scripts/headless-gnome.sh env FOLIO_STARTUP=1 ./target/release/folio README.md
#
# MON=WxH sets the virtual monitor size; SETTLE=seconds waits for the shell.
# Note: GL here is software-rendered, so GPU timings are pessimistic.
exec 3>&2  # keep the app's stderr; D-Bus/shell chatter is discarded below
exec dbus-run-session -- bash -c '
  gnome-shell --headless --virtual-monitor "${MON:-1100x1150}" --wayland-display folio-wl >/dev/null 2>&1 &
  SH=$!
  for i in $(seq 100); do [ -S "$XDG_RUNTIME_DIR/folio-wl" ] && break; sleep 0.1; done
  sleep "${SETTLE:-1}"
  WAYLAND_DISPLAY=folio-wl GDK_BACKEND=wayland timeout 60 "$@" 2>&3
  rc=$?
  kill $SH; wait $SH 2>/dev/null
  exit $rc
' bash "$@" 2>/dev/null
