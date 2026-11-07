#!/usr/bin/env python3
"""Opt-in Niri acceptance test using real Wayland keyboard/pointer events.

Requires Python gi/AT-SPI and wtype, with one active output. Takes focus,
moves the pointer and replaces clipboard contents. File operations use an isolated
temporary Home. NIRI_TONIC_BINARY can select a debug binary. Optional screenshot
paths: NIRI_TONIC_SCREENSHOT and NIRI_TONIC_POPUP_SCREENSHOT.
"""

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, GLib
from wayland_pointer import Pointer

repo = pathlib.Path(__file__).resolve().parent.parent
wtype = os.environ.get("WTYPE") or shutil.which("wtype")
if not wtype or not os.environ.get("NIRI_SOCKET"):
    raise SystemExit("Run inside Niri with wtype installed (or set WTYPE).")
outputs = json.loads(subprocess.check_output(["niri", "msg", "--json", "outputs"]))
if sum(o["current_mode"] is not None for o in outputs.values()) != 1:
    raise SystemExit("This pointer acceptance test requires one active output.")
output = next(o for o in outputs.values() if o["current_mode"] is not None)["logical"]
mouse = Pointer()

fixture = tempfile.TemporaryDirectory(prefix="niri-tonic-input-")
base = pathlib.Path(fixture.name)
root = base / "Home"
(root / "Alpha/Beta").mkdir(parents=True)
(root / "Destination").mkdir()
for name in ["a.txt", "b.txt", "c.txt"]:
    (root / name).write_text(name)
opened = base / "opened.txt"
config = base / "config.yaml"
config.write_text(json.dumps({
    "version": 1,
    "associations": {"extensions": {"txt": [
        sys.executable, "-c",
        "import sys; from pathlib import Path; Path(sys.argv[1]).open('a').write('\\n'.join(sys.argv[2:])+'\\n')",
        str(opened),
    ]}},
}))
state_home = base / "state"
(state_home / "niri-tonic-files").mkdir(parents=True)
(state_home / "niri-tonic-files/session.json").write_text(json.dumps({
    "version": 1, "columns": [(root / "Alpha").as_uri(), (root / "Destination").as_uri()], "active": 1,
}))
env = dict(os.environ, GDK_BACKEND="wayland", NIRI_TONIC_FILES_CONFIG_FILE=str(config),
           HOME=str(root), XDG_STATE_HOME=str(state_home),
           XDG_CONFIG_HOME=os.environ.get("XDG_CONFIG_HOME", str(pathlib.Path.home() / ".config")))
binary = os.environ.get("NIRI_TONIC_BINARY", str(repo / "target/release/niri-tonic-files"))
process = subprocess.Popen([binary], env=env)
loop = GLib.MainLoop()
state = {"step": 0, "error": None, "startup_waits": 0, "pending_click": False, "ready_after": 0}
helpers = []


def objects(obj):
    obj.clear_cache()
    yield obj
    for child in obj:
        yield from objects(child)


def app():
    return next(a for a in Atspi.get_desktop(0) if a.get_process_id() == process.pid)


def own_windows():
    windows = json.loads(subprocess.check_output(["niri", "msg", "--json", "windows"]))
    return [w for w in windows if w.get("pid") == process.pid]


def main_window():
    return next(w for w in own_windows() if w["title"] == "Niri Tonic Files")


def focus():
    own = own_windows()
    if any(w["is_focused"] for w in own):
        return
    window = own[-1]
    subprocess.run(["niri", "msg", "action", "focus-window", "--id", str(window["id"])], check=True)


def keys(*args):
    focus()
    subprocess.run([wtype, *args], check=True)


def ctrl(key):
    keys("-M", "ctrl", "-k", key, "-m", "ctrl")


def current():
    entry = next(o for o in objects(app()) if o.get_name() == "Active folder path")
    return Atspi.Text.get_text(entry, 0, -1)


def pane_paths():
    return [o.get_name().removeprefix("Files in ") for o in objects(app())
            if o.get_role_name() == "list" and o.get_name().startswith("Files in ")]


def listing(path=None):
    name = "Files in " + str(path or current())
    return next(o for o in objects(app()) if o.get_role_name() == "list" and o.get_name() == name)


def row(name, path=None):
    return next(o for o in objects(listing(path)) if o.get_role_name() == "list item" and o.get_name() == name)


def selected(path=None):
    return {o.get_name() for o in objects(listing(path)) if o.get_role_name() == "list item"
            and o.get_state_set().contains(Atspi.StateType.SELECTED)}


def button(name):
    return next(o for o in objects(app()) if o.get_role_name() == "button" and o.get_name() == name)


def activate(name):
    assert button(name).get_action_iface().do_action(0), f"Cannot activate {name}"


def pointer(obj, mouse_button="left", modifiers=(), clicks=1, blank=False, sidebar=False):
    state["pending_click"] = True
    focus()
    rect = Atspi.Component.get_extents(obj, Atspi.CoordType.WINDOW)
    geometry = main_window()["layout"]
    x, y = geometry["tile_pos_in_workspace_view"]
    # GTK reports coordinates within the root box's content allocation. The
    # default Nord root padding is 12 logical pixels, including at 175% scale.
    padding = 24 if sidebar else 12
    x += geometry["window_offset_in_tile"][0] + padding + rect.x + rect.width / 2
    y += geometry["window_offset_in_tile"][1] + padding + rect.y + (rect.height - 20 if blank else rect.height / 2)
    state["last_pointer"] = (obj.get_role_name(), obj.get_name(), rect.x, rect.y, rect.width, rect.height, x, y)
    if modifiers:
        args = [wtype]
        for modifier in modifiers:
            args += ["-M", modifier]
        args += ["-s", "400"]
        for modifier in modifiers:
            args += ["-m", modifier]
        helpers.append(subprocess.Popen(args))

    def click():
        try:
            mouse.click(x, y, output["width"], output["height"], mouse_button, clicks)
            state["last_injection"] = (x, y, mouse_button, clicks)
        except Exception as error:
            state["error"] = error
            loop.quit()
        finally:
            state["pending_click"] = False
            state["ready_after"] = time.monotonic() + 0.3
        return False

    GLib.timeout_add(80, click)


def screenshot(variable, window=None):
    if path := os.environ.get(variable):
        window = window or main_window()
        subprocess.run(["niri", "msg", "action", "screenshot-window", "--id", str(window["id"]),
                        "--path", str(pathlib.Path(path).absolute())], check=True)


def scenario():
    assert current() == str(root) and pane_paths() == [str(root)], "Default startup must ignore old session state"
    window = main_window()
    for args in [["move-window-to-floating"], ["set-window-width", "900"], ["set-window-height", "650"],
                 ["move-floating-window", "--x", "100", "--y", "100"]]:
        subprocess.run(["niri", "msg", "action", *args, "--id", str(window["id"])], check=True)
    yield
    pointer(row("Alpha"), modifiers=("ctrl",))
    yield
    assert pane_paths() == [str(root)] and selected() == {"Alpha"}, "Control-click navigated instead of selecting"
    pointer(row("Alpha"), modifiers=("ctrl",))
    yield
    assert not selected()
    pointer(row("a.txt"), modifiers=("ctrl",))
    yield
    pointer(row("c.txt"), modifiers=("ctrl", "shift"))
    yield
    assert selected() == {"a.txt", "b.txt", "c.txt"}, selected()
    assert not opened.exists()
    pointer(row("b.txt"), modifiers=("shift",))
    yield
    assert selected() == {"b.txt"}, "Shift alone must not select a range"
    for key in ["l", "q", "w", "x", "h", "n", "e", "r"]:
        ctrl(key)
    for key in ["F1", "F2", "F5", "Delete", "BackSpace", "Return"]:
        keys("-k", key)
    keys("-M", "shift", "-k", "Delete", "-k", "F10", "-m", "shift")
    keys("-M", "alt", "-k", "Up", "-m", "alt")
    yield
    assert current() == str(root) and pane_paths() == [str(root)]
    assert (root / "b.txt").exists() and not opened.exists()
    assert len(own_windows()) == 1
    ctrl("d")
    yield
    assert not (root / "b.txt").exists() and len(own_windows()) == 1, "Delete must be immediate, with no dialog"
    pointer(row("a.txt"))
    yield
    assert not opened.exists(), "Single-click opened a file"
    pointer(row("a.txt"), clicks=2)
    yield
    assert opened.read_text().splitlines() == [str(root / "a.txt")]
    pointer(row("Alpha"))
    yield
    assert pane_paths() == [str(root), str(root / "Alpha")]
    pointer(row("Beta"))
    yield
    assert pane_paths() == [str(root / "Alpha/Beta"), str(root / "Alpha")]
    activate("Home")
    yield
    assert pane_paths() == [str(root / "Alpha/Beta"), str(root)]
    ctrl("t")
    yield
    assert pane_paths() == [str(root / "Alpha/Beta")]
    ctrl("t")
    yield
    assert pane_paths() == [str(root / "Alpha/Beta")] * 2
    ctrl("f")
    yield
    pointer(button("Beta"), mouse_button="right", sidebar=True)
    yield
    activate("Open Parent Folder")
    yield
    assert current() == str(root / "Alpha")
    pointer(button("Beta"), mouse_button="right", sidebar=True)
    yield
    activate("Remove from Favorites")
    yield
    assert not any(o.get_role_name() == "button" and o.get_name() == "Beta" for o in objects(app()))
    assert (root / "Alpha/Beta").is_dir()
    activate("Home")
    yield
    pointer(row("a.txt"))
    yield
    assert selected() == {"a.txt"}, selected()
    ctrl("c")
    for number in range(1, 4):
        ctrl("v")
        yield
        assert (root / f"a-{number}.txt").read_text() == "a.txt"
    assert (root / "a.txt").read_text() == "a.txt"
    pointer(row("a-1.txt"), mouse_button="right")
    yield
    activate("Rename…")
    yield
    assert any(w["title"] == "Rename" for w in own_windows()), "Rename dialog missing"
    assert any(o.get_name() == "New name" for o in objects(app()))
    keys("-M", "ctrl", "-k", "a", "-m", "ctrl", "renamed.txt", "-k", "Return")
    yield
    assert (root / "renamed.txt").is_file() and not (root / "a-1.txt").exists()
    pointer(row("renamed.txt"), mouse_button="right")
    yield
    assert not any(o.get_name() == "Delete Permanently…" for o in objects(app()))
    activate("Delete")
    yield
    assert not (root / "renamed.txt").exists() and len(own_windows()) == 1
    pointer(listing(), mouse_button="right", blank=True)
    yield
    activate("New File…")
    yield
    dialog = next(w for w in own_windows() if w["title"] == "New File")
    screenshot("NIRI_TONIC_POPUP_SCREENSHOT", dialog)
    keys("from menu.txt", "-k", "Return")
    yield
    assert (root / "from menu.txt").exists()
    pointer(row("Alpha"), modifiers=("ctrl",))
    yield
    ctrl("f")
    yield
    assert button("Alpha")
    ctrl("f")
    yield
    assert not any(o.get_role_name()=="button" and o.get_name()=="Alpha" for o in objects(app()))
    ctrl("f")
    yield
    ctrl("a")
    yield
    assert selected() == {"Alpha", "Destination", "a.txt", "c.txt", "a-2.txt", "a-3.txt", "from menu.txt"}, selected()
    pointer(listing(), blank=True)
    yield
    assert not selected(), "A blank-space click must clear selection"
    ctrl("a")
    yield
    screenshot("NIRI_TONIC_SCREENSHOT")
    ctrl("Escape")
    yield
    assert process.poll() == 0
    print("WAYLAND PASS: real single/double/Control/range clicks; new shortcuts and disabled legacy keys; numbered paste, rename dialog, direct Delete, favorite menus, column toggle and quit")


steps = scenario()


def tick():
    try:
        if state["pending_click"] or time.monotonic() < state["ready_after"]:
            GLib.timeout_add(150, tick)
            return False
        if state["step"] == 0:
            app()
            current()
            main_window()
        next(steps)
        state["step"] += 1
        # Schedule from completion, not the start of a potentially slow AT-SPI
        # traversal; otherwise the next assertion can overtake a pending click.
        GLib.timeout_add(750, tick)
        return False
    except StopIteration:
        if state["step"] == 0 and process.poll() is None and state["startup_waits"] < 10:
            state["startup_waits"] += 1
            GLib.timeout_add(750, tick)
            return False
        if process.poll() != 0:
            state["error"] = RuntimeError("Scenario ended before successful application exit")
        loop.quit()
        return False
    except Exception as error:
        state["error"] = error
        print("FAIL at step", state["step"], repr(error))
        print("Last pointer:", state.get("last_pointer"))
        print("Last injection:", state.get("last_injection"))
        try:
            print("Visible buttons:", [o.get_name() for o in objects(app()) if o.get_role_name() == "button"])
            screenshot("NIRI_TONIC_FAILURE_SCREENSHOT")
        except Exception:
            pass
        loop.quit()
        return False


GLib.timeout_add(750, tick)
try:
    loop.run()
finally:
    for helper in helpers:
        helper.wait(timeout=5)
    if process.poll() is None:
        process.terminate()
    process.wait(timeout=10)
    fixture.cleanup()
    mouse.close()
if state["error"]:
    raise SystemExit(1)
