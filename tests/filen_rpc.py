"""Isolated protocol peer: no SDK import, credentials, or network access."""
import json
import os
import stat
import sys

instance = None
logged_in = False
items = {}


def reply(frame, **values):
    print(json.dumps({"protocol": 1, "id": frame["id"], **values}), flush=True)


for line in sys.stdin:
    frame = json.loads(line)
    op = frame["op"]
    args = frame.get("args", {})
    if op == "shutdown":
        break
    if op == "hello":
        instance = args["instance"]
        reply(frame, result={"connector": instance})
    elif op == "login":
        # The test uses a public directory path as a synthetic password.
        assert args["password"] == os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        assert "CARGO_MANIFEST_DIR" not in os.environ
        assert args["password"] not in sys.argv
        logged_in = True
        reply(frame, result=None)
    elif op == "cancel":
        reply(frame, error={"code": "cancelled", "message": "fixture", "retryable": False})
    else:
        assert logged_in
        location = {"connector": instance, "id": args["path"]}
        if args["path"] == "file:///malformed":
            print("invalid JSON", flush=True)
        elif args["path"] == "file:///wrong-version":
            print(json.dumps({"protocol": 99, "id": frame["id"], "result": None}), flush=True)
        elif args["path"] == "file:///wait":
            reply(frame, progress={"phase": "Waiting", "bytes": 0, "items": 0, "cancellable": True})
        elif args["path"] == "file:///leak":
            reply(frame, error={"code": "io", "message": "SYNTHETIC_SECRET_MUST_NOT_ESCAPE", "retryable": False})
        elif args["path"] == "file:///expired":
            reply(frame, error={"code": "authentication_required", "message": "SYNTHETIC_SECRET_MUST_NOT_ESCAPE", "retryable": False})
        elif op == "stat":
            reply(frame, result={"location": location, "name": "/", "kind": "directory", "navigable": True,
                                 "hidden": False, "size": os.getpid(), "modified": None, "revision": None, "read_only": False})
        elif op == "list":
            reply(frame, result={"entries": [], "next": None})
        elif op == "write_new":
            assert stat.S_IMODE(os.stat(args["source"]).st_mode) == 0o600
            with open(args["source"], "rb") as source:
                items[args["path"]] = source.read()
            reply(frame, result=None)
        elif op == "read":
            data = items[args["path"]]
            reply(frame, result=list(data[args["offset"]:args["offset"] + args["limit"]]))
        else:
            reply(frame, result=None)
