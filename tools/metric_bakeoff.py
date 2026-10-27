#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Metric bake-off (issue 21): stack one capture once per --metric and compare
the band contrast of the unsharpened stacks over the same part of the Sun.

usage: metric_bakeoff.py HELIA CAPTURE PRESET OUTDIR [--crop 200] [--keep 25%]

Prints a table of the three `helia measure` bands per metric, and the share of
the gradient-energy result. Each stack is measured on a crop centred on its own
fitted disc centre (the `disc:` line of `helia stack`), so the stacks are
compared over the same part of the Sun.
"""
import argparse
import re
import subprocess
from pathlib import Path

METRICS = ["gradient", "laplacian", "helmli", "mfgs", "dct", "fourier", "contrast"]
BANDS = [("fine", r"fine 0-1 px"), ("gran", r"granulation 1-4 px"), ("meso", r"meso 4-12 px")]


def run(cmd):
    return subprocess.run(cmd, capture_output=True, text=True, check=False)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("helia")
    ap.add_argument("capture")
    ap.add_argument("preset")
    ap.add_argument("outdir")
    ap.add_argument("--crop", type=int, default=200)
    ap.add_argument("--keep", default=None)
    a = ap.parse_args()
    out = Path(a.outdir)
    out.mkdir(parents=True, exist_ok=True)
    rows = {}
    for m in METRICS:
        fits = out / f"{m}.fits"
        cmd = [a.helia, "stack", "--backend", "cpu", "--preset", a.preset, "--metric", m, "-o", str(fits)]
        if a.keep:
            cmd += ["--keep", a.keep]
        log = run(cmd + [a.capture]).stderr
        (out / f"{m}.log").write_text(log)
        disc = re.search(r"^disc:\s+centre \(([\d.]+), ([\d.]+)\)", log, re.M)
        mcmd = [a.helia, "measure", str(fits), "--crop", str(a.crop)]
        if disc:
            mcmd += ["--centre", f"{round(float(disc.group(1)))},{round(float(disc.group(2)))}"]
        text = run(mcmd).stdout
        rows[m] = {k: float(re.search(pat + r"\s+([\d.]+)%", text).group(1)) for k, pat in BANDS}
    print("metric     fine%   gran%   meso%   gran/grad  meso/grad")
    g = rows["gradient"]
    for m in METRICS:
        r = rows[m]
        print(f"{m:<10} {r['fine']:6.3f} {r['gran']:7.3f} {r['meso']:7.3f}   {r['gran'] / g['gran']:8.3f}  {r['meso'] / g['meso']:8.3f}")


if __name__ == "__main__":
    main()
