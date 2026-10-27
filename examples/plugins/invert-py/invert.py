#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Invert: a Helia plugin in plain Python, standard library only.

It shows the whole protocol with nothing hidden: JSON-RPC lines on standard input and output,
a reader thread so a cancel is noticed while working, pixels read from and written to files in
the working folder, and errors in Helia's What / Why / What-to-do form. The specification is
the Helia book, Developer guide, "Plugins: design and protocol".

One step, `invert`, an image-op: each value v becomes max + min - v.
"""

import array
import json
import os
import queue
import struct
import sys
import threading

API_VERSION = 1
NAME = "invert-py"
VERSION = "0.1.0"
HEADER = struct.Struct("<4sIIIII")  # magic, layout version, width, height, channels, reserved


class Cancelled(Exception):
    pass


class Failure(Exception):
    """An error to send back, with the three parts Helia shows the user."""

    def __init__(self, code, message, what="", why="", todo=""):
        super().__init__(message)
        self.code, self.what, self.why, self.todo = code, what, why, todo


def send(obj):
    # One JSON value on one line. Standard output carries protocol lines only.
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def log(message):
    # Standard error is the plugin's log; Helia keeps the last 64 KiB.
    print(message, file=sys.stderr, flush=True)


def inside(workdir, rel):
    """Resolve a path from a message; it must stay inside the working folder."""
    if not rel or os.path.isabs(rel) or ".." in rel.replace("\\", "/").split("/"):
        raise Failure(-32602, "Bad buffer path.")
    path = os.path.realpath(os.path.join(workdir, rel))
    if not path.startswith(os.path.realpath(workdir) + os.sep):
        raise Failure(-32602, "Bad buffer path.")
    return path


def read_image(workdir, ref):
    with open(inside(workdir, ref["path"]), "rb") as f:
        raw = f.read()
    magic, version, width, height, channels, _ = HEADER.unpack_from(raw)
    if magic != b"HLPX" or version != 1:
        raise Failure(-32602, "Not an hlpx file.")
    if len(raw) != HEADER.size + 4 * width * height * channels:
        raise Failure(-32602, "The file size does not match its header.")
    data = array.array("f")
    data.frombytes(raw[HEADER.size:])
    if sys.byteorder == "big":
        data.byteswap()
    return width, height, channels, data


def write_image(workdir, out, width, height, channels, data):
    path = inside(workdir, out["path"])
    if sys.byteorder == "big":
        data.byteswap()
    tmp = path + ".part"
    with open(tmp, "wb") as f:
        f.write(HEADER.pack(b"HLPX", 1, width, height, channels, 0))
        f.write(data.tobytes())
    os.replace(tmp, path)  # a crash never leaves a half file under the real name
    return {"path": out["path"], "format": "hlpx", "width": width, "height": height,
            "channels": channels}


class Plugin:
    def __init__(self):
        self.workdir = None
        self.cancelled = threading.Event()
        self.current = None

    def progress(self, fraction, status):
        send({"jsonrpc": "2.0", "method": "progress",
              "params": {"id": self.current, "fraction": fraction, "status": status}})

    def initialize(self, params):
        if params.get("api_version") != API_VERSION:
            raise Failure(-32602, "This plugin speaks API version 1.")
        self.workdir = params["workdir"]
        return {"api_version": API_VERSION, "name": NAME, "version": VERSION,
                "capabilities": ["image-op"], "steps": ["invert"]}

    def image_process(self, params):
        if params.get("step") != "invert":
            raise Failure(-32602, "Unknown step.")
        width, height, channels, data = read_image(self.workdir, params["input"])
        lo, hi = min(data), max(data)
        step = max(len(data) // 20, 1)
        for start in range(0, len(data), step):
            if self.cancelled.is_set():
                raise Cancelled()
            end = min(start + step, len(data))
            data[start:end] = array.array("f", (hi + lo - v for v in data[start:end]))
            self.progress(end / len(data), "Inverting")
        return {"output": write_image(self.workdir, params["output"], width, height, channels,
                                      data)}

    def handle(self, method, params):
        if method == "initialize":
            return self.initialize(params)
        if method == "image.process":
            return self.image_process(params)
        if method == "shutdown":
            return None
        raise Failure(-32601, "Method not found: " + method)


def reader(lines):
    """Read messages in a thread so a `cancel` arrives while a request is being worked on."""
    for line in sys.stdin:
        try:
            msg = json.loads(line)
        except ValueError:
            log("ignored a line that is not JSON")
            continue
        if isinstance(msg, dict) and msg.get("method") == "cancel":
            plugin.cancelled.set()
        else:
            lines.put(msg)
    lines.put(None)


def main():
    lines = queue.Queue()
    threading.Thread(target=reader, args=(lines,), daemon=True).start()
    while True:
        msg = lines.get()
        if msg is None or msg.get("method") == "exit":
            return 0
        if "id" not in msg:
            continue  # a notification we do not know
        plugin.current = msg["id"]
        plugin.cancelled.clear()
        try:
            result = plugin.handle(msg.get("method"), msg.get("params") or {})
            send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
        except Cancelled:
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "error": {"code": -32800, "message": "Cancelled."}})
        except Failure as e:
            error = {"code": e.code, "message": str(e)}
            if e.what:
                error["data"] = {"what": e.what, "why": e.why, "todo": e.todo}
            send({"jsonrpc": "2.0", "id": msg["id"], "error": error})
        except Exception as e:  # a bug in the plugin: say so, stay alive
            log(repr(e))
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "error": {"code": -32603, "message": "Internal error.",
                            "data": {"what": "The invert plugin failed while working.",
                                     "why": "It hit an unexpected condition: " + repr(e)[:200],
                                     "todo": "Report this to the plugin's author."}}})


plugin = Plugin()

if __name__ == "__main__":
    sys.exit(main())
