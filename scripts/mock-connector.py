#!/usr/bin/env python3
"""Offline protocol-1 Filen stand-in. Usage: mock-connector.py INSTANCE ROOT.

No SDK imports, sockets, credential persistence, or account operations. ROOT must
already exist. Optional argv EVENTS CONTROL records operation/path events and
uses CONTROL/<instance>.fail-write to inject one upload failure.
"""
import json
import os
from pathlib import Path
import shutil
import stat
import sys
import time
from urllib.parse import quote, unquote


class ProtocolError(Exception):
    def __init__(self, code):
        self.code = code


class Mock:
    def __init__(self, instance, root, events=None, control=None):
        self.instance = instance
        self.root = Path(root).resolve(strict=True)
        self.read_only = False
        self.ready = False
        self.events = events
        self.control = control

    def event(self, op, path=None):
        if target := self.events:
            with open(target, "a") as output:
                output.write(json.dumps(dict(instance=self.instance, op=op,
                                             path=path, time=time.monotonic_ns())) + "\n")

    def path(self, uri, mutation=False):
        if not isinstance(uri, str) or not uri.startswith("file:///"):
            raise ProtocolError("invalid_input")
        text = unquote(uri[7:], errors="strict")
        parts = text.split("/")[1:]
        if text != "/" and any(p in ("", ".", "..") or "\0" in p for p in parts):
            raise ProtocolError("invalid_input")
        path = self.root.joinpath(*parts)
        if mutation and (self.read_only or path == self.root):
            raise ProtocolError("permission_denied")
        candidate = self.root
        for part in parts:
            candidate /= part
            if candidate.is_symlink():
                raise ProtocolError("unsupported")
        return path

    def location(self, path):
        relative = path.relative_to(self.root).as_posix()
        return dict(connector=self.instance,
                    id="file:///" + quote("" if relative == "." else relative, safe="/"))

    def entry(self, path):
        info = path.lstat()
        if not (stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode)):
            raise ProtocolError("unsupported")
        directory = stat.S_ISDIR(info.st_mode)
        return dict(location=self.location(path), name=path.name,
                    kind="directory" if directory else "file", navigable=directory,
                    hidden=path.name.startswith("."), size=info.st_size,
                    modified=int(info.st_mtime), read_only=self.read_only,
                    revision=f"{info.st_dev}:{info.st_ino}:{info.st_mtime_ns}:{info.st_ctime_ns}")

    @staticmethod
    def absent(path):
        if path.exists() or path.is_symlink():
            raise ProtocolError("already_exists")

    def copy(self, source, destination):
        self.absent(destination)
        if source.is_symlink():
            raise ProtocolError("unsupported")
        if source.is_dir():
            if destination.is_relative_to(source):
                raise ProtocolError("invalid_input")
            destination.mkdir()
            for child in sorted(source.iterdir()):
                self.copy(child, destination / child.name)
        else:
            with source.open("rb") as src, destination.open("xb") as dst:
                shutil.copyfileobj(src, dst, 64 * 1024)

    @staticmethod
    def unique(path, directory=False):
        candidate = path
        number = 0
        while candidate.exists() or candidate.is_symlink():
            number += 1
            stem, suffix = (path.name, "") if directory else (path.stem, path.suffix)
            candidate = path.with_name(f"{stem}-{number}{suffix}")
        return candidate

    def dispatch(self, op, args):
        if op == "hello":
            if args["instance"] != self.instance:
                raise ProtocolError("invalid_input")
            self.read_only = args.get("read_only", False)
            return {"connector": self.instance}
        if op == "login":
            # Synthetic credentials are intentionally neither inspected nor logged.
            self.ready = True
            return None
        if not self.ready:
            raise ProtocolError("authentication_required")
        path = self.path(args["path"], op not in ("stat", "list", "read", "copy", "copy_unique"))
        if op == "stat":
            return self.entry(path)
        if op == "list":
            return dict(entries=[self.entry(p) for p in sorted(path.iterdir())], next=None)
        if op == "read":
            if not 0 <= args["limit"] <= 1024 * 1024 or args["offset"] < 0:
                raise ProtocolError("invalid_input")
            with path.open("rb") as src:
                src.seek(args["offset"])
                return list(src.read(args["limit"]))
        if op == "write_new":
            if control := self.control:
                trigger = Path(control) / f"{self.instance}.fail-write"
                if trigger.exists():
                    trigger.unlink()
                    self.event("write_new_failed", args["path"])
                    raise ProtocolError("io")
            source = Path(args["source"])
            if source.is_symlink() or not source.is_file():
                raise ProtocolError("invalid_input")
            self.copy(source, path)
        elif op == "create_directory":
            path.mkdir()
        elif op in ("rename", "move_item", "copy", "copy_unique"):
            destination = self.path(args["destination"], True)
            if op == "copy_unique":
                destination = self.unique(destination, path.is_dir())
            self.absent(destination)
            if op in ("rename", "move_item"):
                path.rename(destination)
            else:
                self.copy(path, destination)
            if op == "copy_unique":
                return self.location(destination)
        elif op == "delete":
            if path.is_dir():
                shutil.rmtree(path)
            else:
                path.unlink()
        elif op == "archive":
            archive = path.parent / ".archive"
            archive.mkdir(exist_ok=True)
            destination = archive / path.name
            self.absent(destination)
            path.rename(destination)
            return self.location(destination)
        elif op == "restore":
            if path.parent.name != ".archive":
                raise ProtocolError("invalid_input")
            destination = path.parent.parent / path.name
            self.absent(destination)
            path.rename(destination)
            return self.location(destination)
        else:
            raise ProtocolError("unsupported")
        self.event(op + "_commit", args["path"])
        return None


def main():
    mock = Mock(*sys.argv[1:])
    for line in sys.stdin:
        request = json.loads(line)
        if request.get("op") == "shutdown":
            return
        reply = dict(protocol=1, id=request["id"])
        try:
            if request.get("protocol") != 1:
                raise ProtocolError("invalid_input")
            if request["op"] == "cancel":
                raise ProtocolError("cancelled")
            reply["result"] = mock.dispatch(request["op"], request.get("args", {}))
        except Exception as error:
            code = (error.code if isinstance(error, ProtocolError) else
                    "not_found" if isinstance(error, FileNotFoundError) else
                    "already_exists" if isinstance(error, FileExistsError) else
                    "permission_denied" if isinstance(error, PermissionError) else "io")
            reply["error"] = dict(code=code, message="Offline mock operation failed", retryable=False)
        print(json.dumps(reply), flush=True)


if __name__ == "__main__":
    main()
