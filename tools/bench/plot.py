#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Turn the JSON files written by bench.py into SVG graphs and Markdown tables.

    plot.py results/*.json --out docs/book/src/images/perf \
        [--page docs/book/src/user-guide/benchmarks.md]

Needs matplotlib (`python -m venv v && v/bin/pip install matplotlib`).

With --page, each block between `<!-- bench:NAME:begin -->` and
`<!-- bench:NAME:end -->` in the page is replaced by the generated table for NAME,
so the page and the graphs always come from the same data.

Rules applied to every number:

* a run is used only if it succeeded and the machine had under half a core of
  other work while it ran (`other_cores`); runs that fail this are listed, not plotted;
* the figure for a case is the best (lowest wall time) of the remaining runs;
* MB means 10^6 bytes of the capture file on disk.

The same script is kept in Helia, Valerian and Annora.
"""
import argparse
import glob
import json
import os
import re
import sys

import matplotlib

matplotlib.use("svg")
import matplotlib.pyplot as plt  # noqa: E402
from matplotlib.ticker import ScalarFormatter  # noqa: E402

plt.rcParams.update({
    "svg.fonttype": "none", "svg.hashsalt": "bench", "font.size": 10, "axes.spines.top": False,
    "axes.spines.right": False, "axes.grid": True, "grid.alpha": 0.25, "figure.dpi": 100,
    "font.family": "sans-serif", "axes.axisbelow": True,
})
# Okabe-Ito: distinguishable with common colour-vision deficiencies.
PALETTE = ["#0072B2", "#D55E00", "#009E73", "#CC79A7", "#E69F00", "#56B4E9", "#F0E442", "#999999"]
MODE_NAME = {"cpu": "CPU", "gpu": "GPU", "sw": "software Vulkan adapter (lavapipe)",
             "swgl": "software OpenGL adapter (llvmpipe)"}
MODE_ORDER = {"cpu": 0, "gpu": 1, "sw": 2, "swgl": 3}


KERN = []


def load(paths):
    runs, metas = [], {}
    for p in paths:
        d = json.load(open(p))
        label = d["meta"]["label"]
        metas[label] = d["meta"]
        for k in d.get("kernels", []):
            k["platform"] = label
            KERN.append(k)
        for r in d["runs"]:
            r["platform"] = label
            r["series"] = f"{label} {MODE_NAME.get(r['mode'], r['mode'])}"
            runs.append(r)
    return runs, metas


def usable(r):
    return r.get("ok") and not r.get("loaded") and r.get("wall")


def best(runs, **kw):
    sel = [r for r in runs if usable(r) and all(r.get(k) == v for k, v in kw.items())]
    return min(sel, key=lambda r: r["wall"]) if sel else None


def series_list(runs):
    seen = {}
    for r in runs:
        seen[r["series"]] = (r["platform"], MODE_ORDER.get(r["mode"], 9))
    return sorted(seen, key=lambda s: (seen[s][0], seen[s][1]))


def colours(series):
    return {s: PALETTE[i % len(PALETTE)] for i, s in enumerate(series)}


def cases_of(runs, group):
    out = []
    for r in runs:
        if r["group"] == group and r["case"] not in out:
            out.append(r["case"])
    return out


def save(fig, out, name):
    fig.tight_layout()
    fig.savefig(os.path.join(out, name), format="svg", metadata={"Date": None})
    if os.environ.get("BENCH_PNG"):
        fig.savefig(os.path.join(out, name[:-4] + ".png"), format="png", dpi=110)
    plt.close(fig)


def md(header, rows):
    s = "| " + " | ".join(header) + " |\n|" + "---|" * len(header) + "\n"
    return s + "\n".join("| " + " | ".join(str(c) for c in r) + " |" for r in rows) + "\n"


def fmt(x, nd=2):
    return "-" if x is None else f"{x:.{nd}f}"


def graphs(runs, metas, out):
    tables = {}
    series = series_list(runs)
    col = colours(series)
    caps = cases_of(runs, "captures")
    # 1. platform comparison
    if caps:
        fig, ax = plt.subplots(figsize=(max(7, 1.6 * len(caps)), 5.2))
        w = 0.8 / max(1, len(series))
        rows = []
        for i, s in enumerate(series):
            ys = []
            for c in caps:
                b = best(runs, case=c, series=s, group="captures")
                ys.append(b["wall"] if b else 0)
            ax.bar([j + i * w for j in range(len(caps))], ys, w, label=s, color=col[s])
        ax.set_xticks([j + 0.4 - w / 2 for j in range(len(caps))])
        ax.set_xticklabels(caps, rotation=25, ha="right")
        ax.set_yscale("log")
        ax.yaxis.set_major_formatter(ScalarFormatter())
        ax.set_ylabel("seconds, log scale (lower is better)")
        ax.set_title("Wall time per capture, best of N")
        ax.legend(fontsize=8, loc="upper center", bbox_to_anchor=(0.5, -0.32), ncol=2)
        save(fig, out, "platforms.svg")
        for c in caps:
            row = [c]
            for s in series:
                b = best(runs, case=c, series=s, group="captures")
                row.append(fmt(b["wall"]) if b else "-")
            rows.append(row)
        tables["platforms"] = md(["Capture (s)"] + series, rows)
        # 2. stage breakdown for the capture with the longest run
        big = max(caps, key=lambda c: max((r["wall"] for r in runs if r["case"] == c and usable(r) and r["mode"] in ("cpu", "gpu")), default=0))
        stages = []
        for r in runs:
            if r["case"] == big and r["group"] == "captures":
                for k in r["stages"]:
                    if k != "total" and k not in stages:
                        stages.append(k)
        fig, ax = plt.subplots(figsize=(8, 0.6 * len(series) + 2.6))
        srows = []
        for i, s in enumerate(series):
            b = best(runs, case=big, series=s, group="captures")
            if not b:
                continue
            left = 0
            for j, st in enumerate(stages):
                v = b["stages"].get(st, 0)
                ax.barh(s, v, left=left, color=PALETTE[j % len(PALETTE)], label=st if i == 0 else None)
                left += v
            srows.append([s] + [fmt(b["stages"].get(st)) for st in stages] + [fmt(b["wall"])])
        ax.set_xlabel("seconds")
        ax.set_title(f"Where the time goes: {big}")
        ax.legend(fontsize=7, ncol=4, loc="upper center", bbox_to_anchor=(0.5, -0.22))
        ax.invert_yaxis()
        save(fig, out, "stages.svg")
        tables["stages"] = f"Capture: `{big}`.\n\n" + md(["Run (s)"] + stages + ["total"], srows)
        # 3. throughput
        fig, (a1, a2) = plt.subplots(1, 2, figsize=(max(8, 2.2 * len(caps)), 4))
        rows = []
        for i, s in enumerate(series):
            f_, m_ = [], []
            for c in caps:
                b = best(runs, case=c, series=s, group="captures")
                f_.append(b["frames"] / b["wall"] if b and b.get("frames") else 0)
                m_.append(b["bytes"] / 1e6 / b["wall"] if b else 0)
            a1.bar([j + i * w for j in range(len(caps))], f_, w, color=col[s], label=s)
            a2.bar([j + i * w for j in range(len(caps))], m_, w, color=col[s])
        for ax_, t in ((a1, "frames per second"), (a2, "MB per second (capture file size)")):
            ax_.set_xticks([j + 0.4 - w / 2 for j in range(len(caps))])
            ax_.set_xticklabels(caps, rotation=25, ha="right", fontsize=8)
            ax_.set_title(t)
        a1.legend(fontsize=7)
        save(fig, out, "throughput.svg")
        for c in caps:
            row = [c]
            for s in series:
                b = best(runs, case=c, series=s, group="captures")
                row.append(f"{b['frames'] / b['wall']:.0f} / {b['bytes'] / 1e6 / b['wall']:.0f}" if b and b.get("frames") else "-")
            rows.append(row)
        tables["throughput"] = md(["Capture (frames/s / MB/s)"] + series, rows)
        # 4. memory
        fig, ax = plt.subplots(figsize=(max(7, 1.6 * len(caps)), 4))
        rows = []
        for i, s in enumerate(series):
            ax.bar([j + i * w for j in range(len(caps))],
                   [(best(runs, case=c, series=s, group="captures") or {}).get("rss_mb", 0) for c in caps],
                   w, color=col[s], label=s)
        ax.set_xticks([j + 0.4 - w / 2 for j in range(len(caps))])
        ax.set_xticklabels(caps, rotation=25, ha="right")
        ax.set_ylabel("peak resident memory, MB")
        ax.set_title("Peak memory (process, host RAM)")
        ax.legend(fontsize=8)
        save(fig, out, "memory.svg")
        for c in caps:
            row = [c]
            for s in series:
                b = best(runs, case=c, series=s, group="captures")
                row.append(("-" if not b else f"{b['rss_mb']}" + (f" + {b['vram_mb']} VRAM" if b.get("vram_mb") else "")))
            rows.append(row)
        tables["memory"] = md(["Capture (MB)"] + series, rows)
    # 5. thread scaling
    th = [r for r in runs if r["group"] == "threads"]
    if th:
        fig, ax = plt.subplots(figsize=(6.5, 4.2))
        rows = {}
        for s in series_list(th):
            pts = {}
            for r in th:
                if r["series"] == s and usable(r):
                    pts[r["threads"]] = min(pts.get(r["threads"], 1e9), r["wall"])
            if not pts:
                continue
            xs = sorted(pts)
            ax.plot(xs, [pts[x] for x in xs], "o-", color=col.get(s, PALETTE[0]), label=s)
            t1 = pts.get(1)
            if t1:
                ax.plot(xs, [t1 / x for x in xs], "--", color=col.get(s, PALETTE[0]), alpha=0.4,
                        label=f"{s}: ideal 1/n")
            for x in xs:
                rows.setdefault(x, {})[s] = (pts[x], t1 / pts[x] if t1 else None)
        ax.set_xscale("log", base=2)
        ax.set_yscale("log", base=2)
        ax.xaxis.set_major_formatter(ScalarFormatter())
        ax.yaxis.set_major_formatter(ScalarFormatter())
        ax.set_xticks([1, 2, 4, 8])
        ax.set_xlabel("cores")
        ax.set_ylabel("seconds")
        ax.set_title("Scaling with cores")
        ax.legend(fontsize=7)
        save(fig, out, "scaling-cores.svg")
        ss = [s for s in series_list(th)]
        trs = []
        for x in sorted(rows):
            trs.append([x] + [f"{rows[x][s][0]:.2f} ({rows[x][s][1]:.1f}x)" if s in rows[x] and rows[x][s][1] else
                              (fmt(rows[x][s][0]) if s in rows[x] else "-") for s in ss])
        tables["cores"] = md(["Cores: seconds (speed-up over 1 core)"] + ss, trs)
    # 6. frame scaling
    bases = {r["case"].split("@")[0] if "@" in r["case"] else r["case"].rsplit("-", 1)[0]
             for r in runs if r["group"] == "frames"}
    fr = [r for r in runs if r["group"] == "frames" or (r["group"] == "captures" and (r["case"] in bases or r["case"].rsplit("-", 1)[0] in bases))]
    if fr:
        fig, ax = plt.subplots(figsize=(6.5, 4.2))
        rows = {}
        for s in series_list(fr):
            pts = {}
            for r in fr:
                if r["series"] == s and usable(r) and r.get("frames"):
                    pts[r["frames"]] = min(pts.get(r["frames"], 1e9), r["wall"])
            if len(pts) < 1:
                continue
            xs = sorted(pts)
            ax.plot(xs, [pts[x] for x in xs], "o-", color=col.get(s, PALETTE[0]), label=s)
            for x in xs:
                rows.setdefault(x, {})[s] = pts[x]
        ax.set_xlabel("frames in the capture")
        ax.set_ylabel("seconds")
        ax.set_title("Scaling with frame count")
        ax.set_ylim(bottom=0)
        ax.legend(fontsize=8)
        save(fig, out, "scaling-frames.svg")
        ss = series_list(fr)
        tables["frames"] = md(["Frames (s)"] + ss, [[x] + [fmt(rows[x].get(s)) for s in ss] for x in sorted(rows)])
    # 7. kernel benchmark (valerian bench)
    good = [k for k in KERN if k.get("ok")]
    if good:
        fig, axes = plt.subplots(1, len(good), figsize=(5.5 * len(good), 4.2), squeeze=False)
        rows = []
        for ax, k in zip(axes[0], good):
            names = [r["stage"] for r in k["rows"]]
            xs = range(len(names))
            ax.bar([x - 0.2 for x in xs], [r["cpu_ms"] for r in k["rows"]], 0.4, label="CPU", color=PALETTE[0])
            ax.bar([x + 0.2 for x in xs], [r["gpu_ms"] for r in k["rows"]], 0.4,
                   label=MODE_NAME.get(k["mode"], k["mode"]), color=PALETTE[1])
            ax.set_xticks(list(xs))
            ax.set_xticklabels(names, rotation=30, ha="right", fontsize=7)
            ax.set_ylabel("milliseconds (median, lower is better)")
            ax.set_title(f"{k['platform']}: {k['adapter']}", fontsize=9)
            ax.legend(fontsize=8)
            for r in k["rows"]:
                rows.append([k["platform"], MODE_NAME.get(k["mode"], k["mode"]), r["stage"], r["cpu_ms"], r["gpu_ms"],
                             f"{r['speedup']:.1f}x", f"{r['up_ms']:.0f} / {r['kernel_ms']:.0f} / {r['down_ms']:.0f}",
                             r["worst_diff"]])
        save(fig, out, "kernels.svg")
        k0 = good[0]
        tables["kernels"] = (f"Frames of {k0['width']} x {k0['height']}, {k0['frames']} in the combine, "
                             f"median of {k0['repeats']} runs." + chr(10) * 2 +
                             md(["Machine", "GPU side", "Stage", "CPU ms", "GPU ms", "Speed-up",
                                 "Up / kernel / down ms", "Worst diff"], rows))
    # conditions table
    cr = []
    for lab, m in sorted(metas.items()):
        mine = [r for r in runs if r["platform"] == lab]
        loaded = sum(1 for r in mine if r.get("loaded"))
        cr.append([lab, m.get("cpu", "-"), m.get("cores", "-"), m.get("ram_gb", "-"),
                   "; ".join(m.get("gpus", [])) or "-", m.get("version", "-"), m.get("commit", "-"),
                   m.get("date", "-"), m.get("reps", "-"),
                   f"{len(mine)} runs, {loaded} left out as loaded"])
    tables["method"] = md(["Machine", "CPU", "Cores", "RAM GB", "GPU", "Build", "Commit", "Date", "Best of", "Load"], cr)
    return tables


def update_page(page, tables):
    s = open(page, encoding="utf-8").read()
    for name, body in tables.items():
        pat = re.compile(rf"(<!-- bench:{name}:begin -->\n).*?(<!-- bench:{name}:end -->)", re.S)
        if not pat.search(s):
            print(f"note: no block for {name} in {page}", file=sys.stderr)
            continue
        s = pat.sub(lambda m: m.group(1) + body + m.group(2), s)
    open(page, "w", encoding="utf-8", newline="\n").write(s)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("results", nargs="+")
    ap.add_argument("--out", required=True)
    ap.add_argument("--page")
    a = ap.parse_args()
    paths = [p for g in a.results for p in glob.glob(g)]
    runs, metas = load(paths)
    os.makedirs(a.out, exist_ok=True)
    tables = graphs(runs, metas, a.out)
    if a.page:
        update_page(a.page, tables)
    else:
        for k, v in tables.items():
            print(f"## {k}\n{v}")


if __name__ == "__main__":
    main()
