"""Small, persistent virtual pointer for the opt-in desktop acceptance test.

Uses v1 of the public wlr virtual-pointer protocol with absolute placement on one
persistent device, so compositor warps cannot accumulate between clicks.
Protocol: https://gitlab.freedesktop.org/wlroots/wlr-protocols/-/blob/master/unstable/wlr-virtual-pointer-unstable-v1.xml
No file descriptors are transferred by the registry, callback or pointer messages.
"""

import os
import pathlib
import socket
import struct
import time


class Pointer:
    def __init__(self):
        display = pathlib.Path(os.environ["WAYLAND_DISPLAY"])
        if not display.is_absolute():
            display = pathlib.Path(os.environ["XDG_RUNTIME_DIR"]) / display
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(5)
        self.socket.connect(str(display))
        self.next_id = 3
        self.position = None
        self.globals = {}
        self.send(1, 1, struct.pack("=I", 2))  # wl_display.get_registry
        self.sync()
        interface = "zwlr_virtual_pointer_manager_v1"
        if interface not in self.globals:
            self.close()
            raise RuntimeError("The compositor does not expose the virtual-pointer protocol")
        manager = self.allocate()
        name, _version = self.globals[interface]
        encoded = interface.encode() + b"\0"
        string = struct.pack("=I", len(encoded)) + encoded + b"\0" * (-len(encoded) % 4)
        self.send(2, 0, struct.pack("=I", name) + string + struct.pack("=II", 1, manager))
        self.pointer = self.allocate()
        self.send(manager, 0, struct.pack("=II", 0, self.pointer))
        self.sync()

    def allocate(self):
        identifier = self.next_id
        self.next_id += 1
        return identifier

    def send(self, object_id, opcode, payload=b""):
        self.socket.sendall(struct.pack("=II", object_id, ((len(payload) + 8) << 16) | opcode) + payload)

    def receive(self, size):
        result = b""
        while len(result) < size:
            chunk = self.socket.recv(size - len(result))
            if not chunk:
                raise RuntimeError("Wayland connection closed")
            result += chunk
        return result

    def sync(self):
        callback = self.allocate()
        self.send(1, 0, struct.pack("=I", callback))
        while True:
            object_id, header = struct.unpack("=II", self.receive(8))
            opcode, size = header & 0xFFFF, header >> 16
            if size < 8:
                raise RuntimeError("Invalid Wayland event size")
            payload = self.receive(size - 8)
            if object_id == 1 and opcode == 0:
                raise RuntimeError(f"Wayland protocol error: {payload!r}")
            if object_id == callback and opcode == 0:
                return
            if object_id == 2 and opcode == 0:
                name, length = struct.unpack_from("=II", payload)
                interface = payload[8:8 + length - 1].decode()
                version = struct.unpack_from("=I", payload, 8 + ((length + 3) & ~3))[0]
                self.globals[interface] = (name, version)

    def click(self, x, y, width, height, button="left", count=1):
        timestamp = int(time.monotonic() * 1000) & 0xFFFFFFFF
        self.send(self.pointer, 1, struct.pack("=IIIII", timestamp, round(x), round(y), width, height))
        self.position=(x,y)
        self.send(self.pointer, 4)
        self.sync()
        time.sleep(0.03)
        code = {"left": 0x110, "right": 0x111}[button]
        for i in range(count):
            for pressed in (1, 0):
                timestamp = int(time.monotonic() * 1000) & 0xFFFFFFFF
                self.send(self.pointer, 2, struct.pack("=III", timestamp, code, pressed))
                self.send(self.pointer, 4)
                self.sync()
                if pressed:
                    # Give the client a frame to establish a popup grab before
                    # releasing the context-menu button, as a physical click does.
                    time.sleep(0.06)
            if i + 1 < count:
                time.sleep(0.04)

    def close(self):
        self.socket.close()
