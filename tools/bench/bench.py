#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Reproducible `helia stack` benchmark: writes one JSON file per machine.

    bench.py --bin target/release/helia --data DIR --out results/x86.json \
        --modes cpu,sw --reps 3 --threads 1,2,4,8 --scaling

What it measures, for every capture it finds in DIR (see CASES):

* end-to-end wall time, and the wall time of each stage (`--timings`);
* busy CPU time per kind of work (decode, quality, alignment points...);
* peak resident memory of the process (rusage of the child);
* peak video memory (nvidia-smi sampled at 20 Hz, GPU modes on an NVIDIA card);
* the CPU time the machine spent on work that was not ours while the run
  lasted (`other_cores`); a run with more than 0.5 cores of foreign load is
  marked `loaded` and the plots leave it out.

Modes: `cpu` (--backend cpu), `gpu` (--backend gpu on the first real adapter, or
HELIA_GPU_DEVICE), `sw` (--backend gpu on the Mesa lavapipe software adapter).

Scaling: with --threads the first capture is run on 1..N cores (taskset plus
RAYON_NUM_THREADS); with --scaling a SER capture is cut to a quarter, a half and
all of its frames, written next to the data in DIR/.subsets.

A GPU run on a shared host waits until the one-minute load average is under 8
(for up to three hours; --force skips the wait). Nothing here is a statistic of a loaded machine:
the report keeps the best of N and says how loaded the machine was.
"""
import argparse
import json
import os
import platform
import re
import struct
import subprocess
import sys
import time

from benchlib import collect, env_for, gate, machine, run_timed

# name: (file under DIR, preset, description)
CASES = {
    "ha-720x540-avi": ("15_32_09.avi", "ha", "720x540 8-bit Bayer AVI, 1000 frames"),
    "ha-1080-ser16": ("15_20_57.ser", "ha", "1920x1080 16-bit SER, 100 frames"),
    "ha-1080-ser8": ("Sun_Test.ser", "ha", "1920x1080 8-bit SER, 631 frames"),
    "cak-mono-avi": ("15_44_48.avi", "cak", "1936x1088 8-bit mono AVI, 498 frames"),
    "white-bayer-avi": ("14_42_57.avi", "white", "1920x1080 8-bit AVI, 630 frames"),
    "cak-bayer-avi": ("15_36_42.avi", "cak", "1920x1080 8-bit Bayer AVI, 2713 frames"),
}
STAGE_RE = re.compile(r"^  (\w[\w ]*?)\s{2,}([\d.]+)$", re.M)
BUSY_RE = re.compile(r"^  (\w[\w ]*?)\s{2,}([\d.]+)\s+([\d.]+)$", re.M)
FRAMES_RE = re.compile(r"frames:\s+(\d+) total, (\d+) kept, (\d+) registered")


def make_subsets(src, dst_dir):
    """Cut a SER capture to a quarter, half and all of its frames."""
    os.makedirs(dst_dir, exist_ok=True)
    with open(src, "rb") as f:
        head = f.read(178)
        colour, _le, w, h, depth, n = struct.unpack_from("<iiiiii", head, 18)
        fsize = w * h * (1 if depth <= 8 else 2) * (3 if colour in (100, 101) else 1)
        out = {}
        for frac in (4, 2):
            k = n // frac
            name = os.path.join(dst_dir, f"{os.path.basename(src)[:-4]}-{k}.ser")
            if not os.path.exists(name):
                f.seek(178)
                with open(name, "wb") as o:
                    o.write(head[:38] + struct.pack("<i", k) + head[42:])
                    left = k * fsize
                    while left:
                        b = f.read(min(left, 1 << 24))
                        o.write(b)
                        left -= len(b)
            out[k] = name
    return out, n


def run_one(binary, path, preset, mode, threads, gpu_index):
    cmd = [binary, "stack", "--timings", "--preset", preset, "--backend", "cpu" if mode == "cpu" else "gpu",
           "-o", "/tmp/helia-bench-out.tif", path]
    r = run_timed(cmd, env_for(mode, gpu_index), threads, gpu_index, vram=mode == "gpu")
    txt = chr(10).join(l for _, l in r.pop("lines"))
    fr = FRAMES_RE.search(txt)
    stages = {k: float(v) for k, v in STAGE_RE.findall(txt)}
    r.update(stages=stages, busy={k: [float(a), float(b)] for k, a, b in BUSY_RE.findall(txt)},
             frames=int(fr.group(1)) if fr else None, kept=int(fr.group(2)) if fr else None,
             tail=None if r["ok"] else txt[-400:])
    r["ok"] = r["ok"] and "total" in stages
    return r


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", required=True)
    ap.add_argument("--data", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--modes", default="cpu")
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--cases", default=",".join(CASES))
    ap.add_argument("--threads", default="", help="comma list of core counts for the scaling run")
    ap.add_argument("--scaling", action="store_true", help="frame-count scaling on a SER capture")
    ap.add_argument("--gpu-index", type=int, default=None, help="HELIA_GPU_DEVICE and nvidia-smi index")
    ap.add_argument("--commit", default="")
    ap.add_argument("--label", default="")
    ap.add_argument("--force", action="store_true")
    a = ap.parse_args()
    modes = a.modes.split(",")
    jobs = []  # (group, case, path, preset, mode, threads, frames_in_file)
    for c in a.cases.split(","):
        f, preset, desc = CASES[c]
        p = os.path.join(a.data, f)
        if not os.path.exists(p):
            print(f"skip {c}: {p} missing", file=sys.stderr)
            continue
        for m in modes:
            jobs.append(("captures", c, p, preset, m, 0))
    first = next((j for j in jobs), None)
    if a.threads and first:
        c = "ha-1080-ser8" if os.path.exists(os.path.join(a.data, "Sun_Test.ser")) else first[1]
        f, preset, _ = CASES[c]
        for t in [int(x) for x in a.threads.split(",")]:
            for m in modes:
                if m != "gpu":
                    jobs.append(("threads", c, os.path.join(a.data, f), preset, m, t))
    if a.scaling:
        f = os.path.join(a.data, "Sun_Test.ser")
        if os.path.exists(f):
            subs, n = make_subsets(f, os.path.join(a.data, ".subsets"))
            for k, path in sorted(subs.items()):
                for m in modes:
                    jobs.append(("frames", f"ha-1080-ser8@{k}", path, "ha", m, 0))
    runs = []
    for group, case, path, preset, mode, threads in jobs:
        load1 = gate(mode, a.force)
        size = os.path.getsize(path)
        for r, res in enumerate(collect(lambda: run_one(a.bin, path, preset, mode, threads, a.gpu_index), a.reps)):
            res.update(group=group, case=case, mode=mode, threads=threads, rep=r, load1_before=load1, bytes=size)
            runs.append(res)
            print(json.dumps({k: res[k] for k in ("group", "case", "mode", "threads", "rep", "wall",
                                                  "rss_mb", "other_cores", "ok")}), flush=True)
    meta = machine()
    meta.update(label=a.label or platform.node(), commit=a.commit, bin=a.bin, date=time.strftime("%Y-%m-%d %H:%M"),
                version=subprocess.run([a.bin, "--version"], capture_output=True, text=True).stdout.splitlines()[0].strip(),
                reps=a.reps, gpu_index=a.gpu_index)
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    with open(a.out, "w") as f:
        json.dump({"app": "helia", "meta": meta, "cases": {k: v[2] for k, v in CASES.items()}, "runs": runs}, f, indent=1)


if __name__ == "__main__":
    main()
