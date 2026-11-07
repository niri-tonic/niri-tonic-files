"""Offline OAuth protocol peer; never opens a browser or touches token files."""
import json
import os
from pathlib import Path
import sys

state = Path(sys.argv[1])
instance = None
logged_in = False


def reply(frame, **values):
    print(json.dumps({"protocol": 1, "id": frame["id"], **values}), flush=True)


for line in sys.stdin:
    frame = json.loads(line)
    op, args = frame["op"], frame.get("args", {})
    if op == "shutdown":
        break
    if op == "hello":
        assert instance is None
        instance = args["instance"]
        reply(frame, result={"connector": instance})
    elif op == "login":
        assert not logged_in
        assert args["client_id"] == "client-" + instance
        assert args["token_path"] == str(state / "oauth" / instance / "session.json")
        assert Path(args["token_path"]).is_absolute()
        assert args["persist_session"] is (instance == "personal")
        assert "CARGO_MANIFEST_DIR" not in os.environ
        assert "DRIVE_FIXTURE_CLIENT_SECRET" not in os.environ
        assert "UNRELATED_ACCOUNT_SECRET" not in os.environ
        assert "DRIVE_FIXTURE_CHILD" not in os.environ
        if instance == "personal":
            secret = "SYNTHETIC_DRIVE_CLIENT_SECRET"
            assert args["client_secret"] == secret
            assert secret not in sys.argv
            assert secret not in os.environ.values()
        else:
            assert "client_secret" not in args
        for key, value in {
            "DISPLAY": ":fixture", "WAYLAND_DISPLAY": "fixture-wayland",
            "XDG_RUNTIME_DIR": "/fixture-runtime",
            "DBUS_SESSION_BUS_ADDRESS": "unix:path=/fixture-bus",
            "XDG_CURRENT_DESKTOP": "fixture-desktop",
        }.items():
            assert os.environ[key] == value
        reply(frame, progress={"phase": "SYNTHETIC_SECRET", "bytes": 0, "items": 0, "cancellable": True})
        logged_in = True
        reply(frame, result=None)
    else:
        assert logged_in
        if op == "create_directory":
            state.mkdir(exist_ok=True)
            count = state / "mutation-count"
            count.write_text(str(int(count.read_text()) + 1) if count.exists() else "1")
            print("malformed response after mutation", flush=True)
        elif args["path"] == "file:///wrong-id":
            print(json.dumps({"protocol": 1, "id": frame["id"] + 1, "result": None}), flush=True)
        elif args["path"] == "file:///leak":
            reply(frame, error={"code": "io", "message": "SYNTHETIC_SECRET"})
        else:
            reply(frame, result={
                "location": {"connector": instance, "id": args["path"]},
                "name": "/", "kind": "directory", "navigable": True,
                "hidden": False, "size": os.getpid(), "modified": None,
                "revision": None, "read_only": False,
            })
