"""Drive the real app with keyboard and pointer input, then screenshot it.

Run inside the isolated headless session:

    scripts/headless-gnome.sh python3 -I scripts/drive.py OUT.png FILE STEP...

Input is injected through Mutter's RemoteDesktop D-Bus API (real key and
button events, so the input method path is exercised). Each STEP is a
Python expression using the helpers below, for example:

    "click_at(300, 400)" "type('Hello')" "key(ENTER)" "key(ord('z'), CTRL)"

`key()` takes a keysym (`ord('z')`) or a named key (`ENTER`, `F9`, ...).

Coordinates are screen pixels from the top-left corner of the virtual
monitor. Set DELAY (ms, default 5000) to leave time for all steps before
Folio takes its screenshot (FOLIO_SCREENSHOT) and exits.
"""

import os
import subprocess
import sys
import time

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

out, path, steps = sys.argv[1], sys.argv[2], sys.argv[3:]
binary = os.environ.get("FOLIO", "./target/release/folio")
env = dict(os.environ, FOLIO_SCREENSHOT=out, FOLIO_DELAY=os.environ.get("DELAY", "5000"))
app = subprocess.Popen([binary, path], env=env)
time.sleep(1.5)

# The session lives as long as this D-Bus connection.
bus = Gio.bus_get_sync(Gio.BusType.SESSION)


def call(obj, iface, method, args=None):
    return bus.call_sync("org.gnome.Mutter.RemoteDesktop", obj, iface, method, args, None, 0, -1, None)


SESSION = call("/org/gnome/Mutter/RemoteDesktop", "org.gnome.Mutter.RemoteDesktop", "CreateSession").unpack()[0]
IFACE = "org.gnome.Mutter.RemoteDesktop.Session"
call(SESSION, IFACE, "Start")

class Code(int):
    """An evdev keycode. Named keys are sent as keycodes: sent as keysyms,
    keys that also exist on the keypad (End, arrows...) and some others
    (Esc, F9) never reach the app."""


CTRL, SHIFT, ALT = Code(29), Code(42), Code(56)
ENTER, BACK, DELETE, TAB, ESC = Code(28), Code(14), Code(111), Code(15), Code(1)
LEFT, UP, RIGHT, DOWN, HOME, END = Code(105), Code(103), Code(106), Code(108), Code(102), Code(107)
F9 = Code(67)


def sym(k, down):
    if isinstance(k, Code):
        call(SESSION, IFACE, "NotifyKeyboardKeycode", GLib.Variant("(ub)", (k, down)))
    else:
        call(SESSION, IFACE, "NotifyKeyboardKeysym", GLib.Variant("(ub)", (k, down)))


def key(k, *mods):
    """Press and release keysym `k` with modifier keysyms held."""
    for m in mods:
        sym(m, True)
    sym(k, True)
    sym(k, False)
    for m in reversed(mods):
        sym(m, False)
    time.sleep(0.03)


def type(text):  # noqa: A001 - reads naturally in step expressions
    for c in text:
        key(ENTER if c == "\n" else ord(c) if ord(c) < 0x100 else 0x1000000 + ord(c))


def move(dx, dy):
    call(SESSION, IFACE, "NotifyPointerMotionRelative", GLib.Variant("(dd)", (dx, dy)))
    time.sleep(0.05)


# Virtual monitor width (scripts/headless-gnome.sh: MON=WxH).
SCREEN_W = int(os.environ.get("MON", "1100x1150").split("x")[0])


def at(x, y):
    """Move the pointer to screen position (x, y). It is parked in the
    top-right corner first: the top-left one opens the Activities overview,
    which then takes the input."""
    move(10000, -10000)
    move(x - (SCREEN_W - 1), y)
    time.sleep(0.3)


def click(n=1):
    for _ in range(n):
        call(SESSION, IFACE, "NotifyPointerButton", GLib.Variant("(ib)", (272, True)))
        # Without a pause the press is lost.
        time.sleep(0.05)
        call(SESSION, IFACE, "NotifyPointerButton", GLib.Variant("(ib)", (272, False)))
    time.sleep(0.1)


def right_click_at(x, y):
    at(x, y)
    call(SESSION, IFACE, "NotifyPointerButton", GLib.Variant("(ib)", (273, True)))
    time.sleep(0.05)
    call(SESSION, IFACE, "NotifyPointerButton", GLib.Variant("(ib)", (273, False)))
    time.sleep(0.1)


def click_at(x, y, n=1):
    at(x, y)
    click(n)


for step in steps:
    eval(step)
    time.sleep(0.05)
sys.exit(app.wait())
