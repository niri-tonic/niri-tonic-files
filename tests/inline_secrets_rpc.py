"""Offline peer for explicitly inline synthetic credentials; no service imports."""
import json
import os
import sys

instance = None
ready = False
password = "  SYNTHETIC_PASSWORD:'\"\\$\n☃  "
client_secret = "SYNTHETIC_CLIENT_SECRET:#'\"\\\n"

for line in sys.stdin:
    frame = json.loads(line)
    op, args = frame["op"], frame.get("args", {})
    if op == "shutdown":
        break
    reply = dict(protocol=1, id=frame["id"])
    if op == "hello":
        instance = args["instance"]
        reply["result"] = {"connector": instance}
    elif op == "login":
        for value in [password, client_secret, "012345"]:
            assert value not in sys.argv
            assert value not in os.environ.values()
        if instance == "filen":
            assert args["password"] == password
            assert args["twoFactorCode"] == "012345"
        else:
            assert args["client_secret"] == client_secret
        ready = True
        reply["result"] = None
    elif op == "list":
        assert ready
        reply["result"] = {"entries": [], "next": None}
    else:
        assert ready
        # The host must not show even a credential-containing backend error.
        reply["error"] = {"code": "authentication_required", "message": password + client_secret + "012345"}
    print(json.dumps(reply), flush=True)
