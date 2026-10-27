#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Stats: an `analyse` plugin in plain Python, standard library only.

It reads an image and reports numbers and shapes; it changes nothing. The step `stats` returns
the mean, minimum, maximum and standard deviation as metrics, and draws a rectangle round the
bright part of the picture (everything above a share of the maximum, half by default) and a circle at its centre.
Pixels are read from the file the host names; the protocol is in the Helia book, Developer
guide, "Plugins: design and protocol".
"""

import array
import json
import math
import os
import queue
import struct
import sys
import threading

API_VERSION = 1
NAME = "stats-py"
VERSION = "0.1.0"
HEADER = struct.Struct("<4sIIIII")


class Cancelled(Exception):
    pass


class Failure(Exception):
    def __init__(self, code, message, what="", why="", todo=""):
        super().__init__(message)
        self.code, self.what, self.why, self.todo = code, what, why, todo


def send(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def log(message):
    print(message, file=sys.stderr, flush=True)


def inside(workdir, rel):
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
                "capabilities": ["analyse"], "steps": ["stats"]}

    def analyse(self, params):
        if params.get("step") != "stats":
            raise Failure(-32602, "Unknown step.")
        width, height, channels, data = read_image(self.workdir, params["input"])
        n = len(data)
        mean = sum(data) / n
        low, high = min(data), max(data)
        std = math.sqrt(sum((v - mean) ** 2 for v in data) / n)
        self.progress(0.5, "Finding the bright part")
        # The bright part: rows and columns holding a value above `threshold` times the maximum, found on
        # every fourth pixel so a big picture takes seconds, not minutes.
        share = float((params.get("params") or {}).get("threshold", 0.5))
        threshold = high * share
        stride = 4
        xs, ys = [], []
        for y in range(0, height, stride):
            if self.cancelled.is_set():
                raise Cancelled()
            row = data[y * width * channels:(y + 1) * width * channels:channels * stride]
            hits = [x * stride for x, v in enumerate(row) if v > threshold]
            if hits:
                ys.append(y)
                xs.extend((hits[0], hits[-1]))
        result = {"metrics": {"mean": mean, "min": low, "max": high, "std": std},
                  "overlays": [], "notes": []}
        if xs:
            x0, x1, y0, y1 = min(xs), max(xs), min(ys), max(ys)
            result["overlays"] = [
                {"kind": "rect", "x": x0, "y": y0, "w": x1 - x0, "h": y1 - y0,
                 "label": "bright part"},
                {"kind": "circle", "x": (x0 + x1) // 2, "y": (y0 + y1) // 2, "r": 6},
            ]
            result["notes"].append("The bright part is %d by %d pixels." % (x1 - x0, y1 - y0))
        else:
            result["notes"].append("Nothing is brighter than the threshold.")
        return result

    def handle(self, method, params):
        if method == "initialize":
            return self.initialize(params)
        if method == "analyse.run":
            return self.analyse(params)
        if method == "shutdown":
            return None
        raise Failure(-32601, "Method not found: " + method)


def reader(lines):
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
            continue
        plugin.current = msg["id"]
        plugin.cancelled.clear()
        try:
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "result": plugin.handle(msg.get("method"), msg.get("params") or {})})
        except Cancelled:
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "error": {"code": -32800, "message": "Cancelled."}})
        except Failure as e:
            error = {"code": e.code, "message": str(e)}
            if e.what:
                error["data"] = {"what": e.what, "why": e.why, "todo": e.todo}
            send({"jsonrpc": "2.0", "id": msg["id"], "error": error})
        except Exception as e:
            log(repr(e))
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "error": {"code": -32603, "message": "Internal error.",
                            "data": {"what": "The stats plugin failed while working.",
                                     "why": "It hit an unexpected condition: " + repr(e)[:200],
                                     "todo": "Report this to the plugin's author."}}})


plugin = Plugin()

if __name__ == "__main__":
    sys.exit(main())
