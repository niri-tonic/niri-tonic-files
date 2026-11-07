#!/usr/bin/env python3
"""Opt-in, offline Niri connector acceptance test (takes focus/clipboard).

Run sequentially with other GUI tests: python3 scripts/check-connectors.py.
Requires gi/AT-SPI, wtype, Niri with one active output. NIRI_TONIC_BINARY
selects the binary; NIRI_TONIC_CONNECTORS_SCREENSHOT and
NIRI_TONIC_FAILURE_SCREENSHOT optionally retain screenshots. --protocol-only
checks the mock subprocess without importing GUI libraries or touching Wayland.
All account data, HOME, configuration, state and cache are temporary.
"""
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parent.parent
MOCK = Path(__file__).with_name("mock-connector.py")


def protocol_check():
    with tempfile.TemporaryDirectory(prefix="niri-connector-protocol-") as temporary:
        base = Path(temporary)
        root = base / "account"
        root.mkdir()
        source = base / "upload"
        source.write_bytes(b"offline\0payload")
        process = subprocess.Popen([sys.executable, str(MOCK), "test", str(root)],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        request_id = 0

        def request(op, **args):
            nonlocal request_id
            request_id += 1
            process.stdin.write(json.dumps(dict(protocol=1, id=request_id, op=op, args=args)) + "\n")
            process.stdin.flush()
            assert select.select([process.stdout], [], [], 5)[0], "Mock protocol response timed out"
            reply = json.loads(process.stdout.readline())
            assert reply["protocol"] == 1 and reply["id"] == request_id
            return reply

        try:
            assert request("hello", instance="test", read_only=False)["result"] == {"connector": "test"}
            request("login", email="synthetic@example.invalid", password="synthetic")
            assert request("write_new", path="file:///a.txt", source=str(source))["result"] is None
            assert request("read", path="file:///a.txt", offset=0, limit=100)["result"] == list(source.read_bytes())
            assert request("stat", path="file:///a.txt")["result"]["revision"]
            assert request("write_new", path="file:///a.txt", source=str(source))["error"]["code"] == "already_exists"
            assert request("copy_unique", path="file:///a.txt", destination="file:///a.txt")["result"]["id"] == "file:///a-1.txt"
            request("rename", path="file:///a-1.txt", destination="file:///renamed.txt")
            archived = request("archive", path="file:///renamed.txt")["result"]
            assert archived["id"] == "file:///.archive/renamed.txt"
            assert request("restore", path=archived["id"])["result"]["id"] == "file:///renamed.txt"
            request("create_directory", path="file:///folder")
            request("move_item", path="file:///renamed.txt", destination="file:///folder/renamed.txt")
            assert len(request("list", path="file:///folder")["result"]["entries"]) == 1
            request("delete", path="file:///folder", mode="permanent")
            assert not (root / "folder").exists()
            request("hello", instance="test", read_only=True)
            assert request("create_directory", path="file:///blocked")["error"]["code"] == "permission_denied"
            assert request("stat", path="file:///../escape")["error"]["code"] == "invalid_input"
            print("MOCK PROTOCOL PASS: exclusive writes, revision, reads, copy numbering, rename/archive/restore/move/delete, read-only and path isolation")
        finally:
            process.stdin.close()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            process.stdout.close()


def gui_check(mixed=False):
    import gi
    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi, GLib
    from wayland_pointer import Pointer

    wtype = os.environ.get("WTYPE") or shutil.which("wtype")
    if not wtype or not os.environ.get("NIRI_SOCKET"):
        raise SystemExit("Run inside Niri with wtype installed (or set WTYPE).")
    outputs = json.loads(subprocess.check_output(["niri", "msg", "--json", "outputs"]))
    active = [o for o in outputs.values() if o["current_mode"] is not None]
    if len(active) != 1:
        raise SystemExit("Connector pointer checks require exactly one active output.")
    output = active[0]["logical"]
    binary = os.environ.get("NIRI_TONIC_BINARY", str(REPO / "target/release/niri-tonic-files"))
    process = None
    mouse = Pointer()
    step = "fixture"
    with tempfile.TemporaryDirectory(prefix="niri-tonic-connectors-") as temporary:
        base = Path(temporary)
        home = base / "Home"
        for relative in ["Home/Folder/nested", "Home/Returned", "accounts/a", "accounts/b", "accounts/readonly", "accounts/disabled", "config", "state", "cache", "control"]:
            (base / relative).mkdir(parents=True, exist_ok=True)
        payload = b"connector roundtrip\n" * 100
        (home / "note.txt").write_bytes(payload)
        (home / "Folder/nested/deep.txt").write_bytes(payload + b"nested")
        (base / "accounts/a/move.txt").write_bytes(b"move only after commit")
        (base / "accounts/readonly/visible.txt").write_text("read only")
        opened = base / "opened.json"
        events = base / "events.jsonl"
        config = base / "config/config.yaml"
        instances = []
        for instance, name in [("a", "Account Alpha"), ("b", "Account Beta"), ("readonly", "Read Only"), ("disabled", "Disabled Account")]:
            drive = mixed and instance != "a"
            if drive:
                credentials = {"oauth": {"client_id": f"offline-{instance}.apps.googleusercontent.com", "persist_session": True}}
            else:
                credentials = {"auth": dict(email=f"{instance}@example.invalid", password={"env": "NIRI_MOCK_PASSWORD"})}
            instances.append(dict(id=instance, name=name, type="google_drive" if drive else "filen", enabled=instance != "disabled",
                                  read_only=instance == "readonly",
                                  **credentials,
                                  command=["/usr/bin/python3", str(MOCK), instance, str(base / "accounts" / instance), str(events), str(base / "control")]))
        second_type = "Google Drive" if mixed else "Filen"
        scheme = "drive" if mixed else "filen"
        second_root = f"{scheme}://b/"
        readonly_root = f"{scheme}://readonly/"
        favorite_root = second_root if mixed else "filen://a/"
        operations_root = second_root if mixed else "filen://a/"
        operations_folder = base / "accounts" / ("b" if mixed else "a")
        config.write_text(json.dumps(dict(version=1, connectors=dict(instances=instances),
            navigation=dict(animations=False), appearance=dict(follow_dank=False),
            associations=dict(extensions={"txt": ["/usr/bin/python3", "-c",
                "import json,sys; from pathlib import Path; Path(sys.argv[1]).write_text(json.dumps(sys.argv[2:]))", str(opened)]}))))
        env = dict(os.environ, GDK_BACKEND="wayland", HOME=str(home),
                   XDG_CONFIG_HOME=str(base / "config"), XDG_STATE_HOME=str(base / "state"),
                   XDG_CACHE_HOME=str(base / "cache"), NIRI_TONIC_FILES_CONFIG_FILE=str(config),
                   NIRI_MOCK_PASSWORD="synthetic-offline-only")

        def pause(seconds=0.3):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                context = GLib.MainContext.default()
                while context.pending():
                    context.iteration(False)
                time.sleep(0.02)

        def wait(predicate, label, timeout=15):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise AssertionError(f"Application exited during {label}: {process.returncode}")
                try:
                    result = predicate()
                    if result:
                        return result
                except (StopIteration, GLib.Error):
                    pass
                pause(0.1)
            raise AssertionError(f"Timed out: {label}")

        def objects(obj):
            if obj is None:
                return  # AT-SPI can expose a removed child while a dialog closes.
            obj.clear_cache()
            yield obj
            for child in obj:
                yield from objects(child)

        def app():
            return next(a for a in Atspi.get_desktop(0) if a.get_process_id() == process.pid)

        def windows():
            return [w for w in json.loads(subprocess.check_output(["niri", "msg", "--json", "windows"])) if w.get("pid") == process.pid]

        def window():
            return next(w for w in windows() if w["title"] == "Niri Tonic Files")

        def focus():
            own = windows()
            if not any(w["is_focused"] and w["layout"]["tile_pos_in_workspace_view"] is not None for w in own):
                subprocess.run(["niri", "msg", "action", "close-overview"], check=True)
                subprocess.run(["niri", "msg", "action", "focus-window", "--id", str(own[-1]["id"])], check=True)

        def keys(*args):
            focus()
            subprocess.run([wtype, *args], check=True)
            pause()

        def ctrl(key):
            keys("-M", "ctrl", "-k", key, "-m", "ctrl")

        def find(name, role=None, parent=None):
            return next(o for o in objects(parent or app()) if o.get_name() == name and (role is None or o.get_role_name() == role))

        def button(name):
            return wait(lambda: find(name, "button"), f"button {name}")

        def activate(name):
            target = button(name)
            assert target.get_state_set().contains(Atspi.StateType.SENSITIVE), f"Disabled: {name}"
            assert target.get_action_iface().do_action(0), f"Cannot activate {name}"
            pause()

        def current():
            return Atspi.Text.get_text(find("Active folder path"), 0, -1)

        def listing():
            return find("Files in " + current(), "list")

        def row(name):
            return wait(lambda: find(name, "list item", listing()), f"row {name}")

        def pointer(obj, right=False, control=False, clicks=1, blank=False):
            focus()
            rect = Atspi.Component.get_extents(obj, Atspi.CoordType.WINDOW)
            layout = wait(lambda: window()["layout"] if window()["layout"]["tile_pos_in_workspace_view"] is not None else None,
                          "visible window layout")
            x, y = layout["tile_pos_in_workspace_view"]
            x += layout["window_offset_in_tile"][0] + 12 + rect.x + rect.width / 2
            y += layout["window_offset_in_tile"][1] + 12 + rect.y + (rect.height - 20 if blank else rect.height / 2)
            helper = None
            try:
                if control:
                    helper = subprocess.Popen([wtype, "-M", "ctrl", "-s", "400", "-m", "ctrl"])
                    pause(0.08)
                mouse.click(x, y, output["width"], output["height"], "right" if right else "left", clicks)
            finally:
                if helper:
                    helper.wait(timeout=5)
            pause()

        def go(path):
            path = str(path)
            pointer(find("Active folder path"))
            keys("-M", "ctrl", "-k", "a", "-m", "ctrl", path, "-k", "Return")
            wait(lambda: current() == path and listing(), f"navigate {path}")
            pause()

        def select(name):
            pointer(listing(), blank=True)
            pointer(row(name), control=True)
            wait(lambda: row(name).get_state_set().contains(Atspi.StateType.SELECTED), f"select {name}")

        def menu(name, action):
            pointer(row(name), right=True)
            activate(action)

        def copied(name, destination, expected):
            select(name)
            ctrl("c")
            go(destination)
            # Return from the path entry to the list before application shortcuts.
            pointer(listing(), blank=True)
            ctrl("v")
            wait(lambda: expected.exists(), f"copy {expected.name}")
            row(expected.name)

        def screenshot(variable):
            if path := os.environ.get(variable):
                subprocess.run(["niri", "msg", "action", "screenshot-window", "--id", str(window()["id"]),
                                "--path", str(Path(path).absolute())], check=True)

        def start():
            nonlocal process
            subprocess.run(["niri", "msg", "action", "close-overview"], check=True)
            process = subprocess.Popen([binary], env=env)
            wait(lambda: app() and window() and current() == str(home), "startup", timeout=25)
            for args in [["move-window-to-floating"], ["set-window-width", str(min(1000, output["width"] - 80))],
                         ["set-window-height", str(min(800, output["height"] - 80))],
                         ["move-floating-window", "--x", "40", "--y", "30"]]:
                subprocess.run(["niri", "msg", "action", *args, "--id", str(window()["id"])], check=True)
            pause(0.5)

        try:
            start()
            step = "sidebar account identities"
            for name in ["Account Alpha · Filen", f"Account Beta · {second_type}", f"Read Only · {second_type}"]:
                button(name)
            assert not any("Disabled Account" in o.get_name() for o in objects(app()))
            activate("Account Alpha · Filen")
            wait(lambda: current() == "filen://a/", "account A root")
            step = "local → account A → account B → local file and folder"
            for name in ["note.txt", "Folder"]:
                go(home)
                copied(name, "filen://a/", base / "accounts/a" / name)
                copied(name, second_root, base / "accounts/b" / name)
                copied(name, home / "Returned", home / "Returned" / name)
            assert (home / "Returned/note.txt").read_bytes() == payload
            assert (home / "Returned/Folder/nested/deep.txt").read_bytes() == payload + b"nested"
            step = "same-account repeated-copy numbering"
            go(operations_root)
            select("note.txt")
            ctrl("c")
            for number in [1, 2]:
                ctrl("v")
                wait(lambda: (operations_folder / f"note-{number}.txt").exists(), "numbered paste")
                row(f"note-{number}.txt")
            step = "remote rename, archive, restore, delete"
            menu("note-1.txt", "Rename…")
            wait(lambda: any(w["title"] == "Rename" for w in windows()), "rename dialog")
            keys("-M", "ctrl", "-k", "a", "-m", "ctrl", "renamed.txt", "-k", "Return")
            row("renamed.txt")
            menu("renamed.txt", "Archive")
            wait(lambda: (operations_folder / ".archive/renamed.txt").exists(), "archive")
            go(operations_root + ".archive")
            menu("renamed.txt", "Restore from Archive")
            wait(lambda: (operations_folder / "renamed.txt").exists(), "restore")
            go(operations_root)
            select("renamed.txt")
            ctrl("d")
            wait(lambda: not (operations_folder / "renamed.txt").exists(), "delete")
            step = "failed then successful cross-account cut/paste"
            go("filen://a/")
            menu("move.txt", "Cut")
            go(second_root)
            trigger = base / "control/b.fail-write"
            trigger.touch()
            pointer(listing(), blank=True)
            ctrl("v")
            wait(lambda: not trigger.exists(), "injected upload failure")
            pause(0.8)
            assert (base / "accounts/a/move.txt").exists() and not (base / "accounts/b/move.txt").exists()
            ctrl("v")
            wait(lambda: (base / "accounts/b/move.txt").exists() and not (base / "accounts/a/move.txt").exists(), "successful move")
            log = [json.loads(line) for line in events.read_text().splitlines()]
            uploaded = next(e["time"] for e in log if e["instance"] == "b" and e["op"] == "write_new_commit" and e["path"] == "file:///move.txt")
            deleted = next(e["time"] for e in log if e["instance"] == "a" and e["op"] == "delete_commit" and e["path"] == "file:///move.txt")
            assert uploaded < deleted
            step = "read-only destination blocks paste"
            select("note.txt")
            ctrl("c")
            activate(f"Read Only · {second_type}")
            wait(lambda: current() == readonly_root, "read-only root")
            pointer(row("visible.txt"), right=True)
            assert not button("Paste").get_state_set().contains(Atspi.StateType.SENSITIVE)
            keys("-k", "Escape")
            pointer(listing(), blank=True)
            ctrl("v")
            pause(0.5)
            assert sorted(p.name for p in (base / "accounts/readonly").iterdir()) == ["visible.txt"]
            step = "remote opening uses private account materialization"
            go(second_root)
            pointer(row("note.txt"), clicks=2)
            wait(opened.exists, "recording opener")
            materialized, = json.loads(opened.read_text())
            downloaded = Path(materialized)
            assert downloaded.is_relative_to(base / "cache/niri-tonic-files/materialized/b")
            assert downloaded.read_bytes() == payload
            assert downloaded.parent.stat().st_mode & 0o777 == 0o700
            step = "remote favorite persistence and bounded parent menu"
            go(favorite_root)
            select("Folder")
            ctrl("f")
            button("Folder")
            wait(lambda: favorite_root + "Folder" in config.read_text(), "persist remote favorite URI")
            activate("Folder")
            wait(lambda: current() == favorite_root + "Folder", "remote favorite")
            screenshot("NIRI_TONIC_CONNECTORS_SCREENSHOT")
            # Restart proves the saved URI is consumed, not just an in-memory favorite.
            pointer(listing(), blank=True)
            ctrl("Escape")
            process.wait(timeout=10)
            assert process.returncode == 0
            start()
            activate("Folder")
            wait(lambda: current() == favorite_root + "Folder", "favorite after restart")
            # Sidebar context menu exposes the connector-bounded parent helper.
            target = button("Folder")
            rect = Atspi.Component.get_extents(target, Atspi.CoordType.WINDOW)
            layout = window()["layout"]
            x, y = layout["tile_pos_in_workspace_view"]
            mouse.click(x + layout["window_offset_in_tile"][0] + 24 + rect.x + rect.width / 2,
                        y + layout["window_offset_in_tile"][1] + 24 + rect.y + rect.height / 2,
                        output["width"], output["height"], "right", 1)
            pause()
            activate("Open Parent Folder")
            wait(lambda: current() == favorite_root, "favorite parent")
            pointer(listing(), blank=True)
            ctrl("Escape")
            process.wait(timeout=10)
            assert process.returncode == 0
            print(f"CONNECTORS GUI PASS (Filen + {second_type}): two offline accounts, recursive roundtrip, numbering, rename/archive/restore/delete, failed/successful move ordering, read-only/disabled instances, private open and persisted favorite")
        except Exception:
            print(f"CONNECTORS GUI FAIL: {step}", file=sys.stderr)
            if process and process.poll() is None:
                try:
                    print("Fixture windows:", windows(), file=sys.stderr)
                    if events.exists():
                        print("Last fixture operations:", events.read_text().splitlines()[-8:], file=sys.stderr)
                    print("Fixture accessibility:", [(o.get_role_name(), o.get_name()) for o in objects(app())
                        if o.get_name().startswith(("Files in", "Active folder"))], file=sys.stderr)
                except Exception:
                    pass
                try:
                    screenshot("NIRI_TONIC_FAILURE_SCREENSHOT")
                except Exception:
                    pass
            raise
        finally:
            if process and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            mouse.close()


if __name__ == "__main__":
    if sys.argv[1:] == ["--protocol-only"]:
        protocol_check()
    elif sys.argv[1:] == ["--mixed-providers"]:
        gui_check(mixed=True)
    elif sys.argv[1:]:
        raise SystemExit("Usage: check-connectors.py [--protocol-only|--mixed-providers]")
    else:
        gui_check()
