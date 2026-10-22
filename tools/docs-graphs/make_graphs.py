#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Render the book's measured-result charts from small data files, and check they are current.

    python3 tools/docs-graphs/make_graphs.py            # write docs/book/src/images/charts/*.svg
    python3 tools/docs-graphs/make_graphs.py --check    # fail if a chart is stale, or a number is not on its page

Each chart is one JSON file in docs/book/src/data/charts/. The file holds the numbers
(copied from the table or prose of the page named in "page"), the chart type, the axis
labels, a one-line caption and the alt text. The table stays on the page; the chart is its
picture. Standard library only, so the output is byte-for-byte reproducible and CI needs no
plotting package. The same script is kept in Helia, Valerian and Annora.

Chart file fields:
  id        file name of the chart (also the SVG name)
  page      page of the book (relative to docs/book/src) that holds the table or the prose
  kind      "bars" (vertical groups), "hbars" (horizontal groups, for long labels),
            "dumbbell" (before and after per row, two series), "lines", "scatter"
  title     short title drawn in the picture
  caption   one line for under the picture: what it shows and where the numbers came from
  alt       alt text for the picture
  categories  names of the groups (bars, hbars, dumbbell)
  series    [{"name", "values": [...]}]; for lines and scatter also "x": [...]
  value_axis  label with units for the value axis ("ylabel" for lines and scatter)
  xlabel    label for the x axis of lines and scatter
  dp        decimals on the value labels (default 2)
  log       true for a log value axis (y for lines and scatter)
  logx      true for a log x axis (lines and scatter)
  labels    false to drop the value labels on bars (default true)
  equal_axes     scatter: the same range on both axes (square plot), for agreement charts
  identity_line  scatter: a dashed y = x line (points on it agree exactly)
  points    scatter only: [{"x", "y", "label"}] to name the dots (optional)
  note      where the numbers came from, kept in the file for the maintainer

Every number in "series" (and "x", "points") must appear on the page, so a chart cannot
quote a figure the page does not.

Style: Okabe-Ito colours (distinguishable with common colour-vision deficiencies), a card
that carries its own background so it reads in any book theme, light and dark through
prefers-color-scheme, text at 13 px in a 480 px wide picture so it stays legible when scaled
to a phone.
"""
import argparse
import html
import json
import math
import re
import sys
from pathlib import Path

PALETTE = ["#0072B2", "#D55E00", "#009E73", "#CC79A7", "#E69F00", "#56B4E9", "#F0E442", "#999999"]
W = 480
FS = 13
CHAR = 7.0  # average glyph width at FS, for wrapping
STYLE = """<style>
.bg{fill:#ffffff;stroke:#d0d7de}
.ink{fill:#1f2328}.sub{fill:#59636e}
.grid{stroke:#d8dee4;stroke-width:1}.axis{stroke:#59636e;stroke-width:1}
.ln{fill:none}
text{font-family:system-ui,-apple-system,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;font-size:13px}
.t{font-size:14px;font-weight:600}
@media (prefers-color-scheme:dark){.bg{fill:#161b22;stroke:#30363d}.ink{fill:#e6edf3}.sub{fill:#9da7b3}
.grid{stroke:#30363d}.axis{stroke:#9da7b3}}
</style>"""


def esc(s):
    return html.escape(str(s), quote=True)


def wrap(text, width_px):
    """Break text into lines no wider than width_px (estimate)."""
    n = max(6, int(width_px / CHAR))
    words, lines, cur = str(text).split(), [], ""
    for w in words:
        while len(w) > n:
            if cur:
                lines.append(cur)
                cur = ""
            lines.append(w[:n])
            w = w[n:]
        if len(cur) + len(w) + (1 if cur else 0) <= n:
            cur = (cur + " " + w).strip()
        else:
            lines.append(cur)
            cur = w
    if cur:
        lines.append(cur)
    return lines or [""]


def nice_ticks(lo, hi, want=5):
    if hi <= lo:
        hi = lo + 1
    raw = (hi - lo) / want
    mag = 10 ** math.floor(math.log10(raw))
    for m in (1, 2, 2.5, 5, 10):
        step = m * mag
        if (hi - lo) / step <= want + 1:
            break
    a = math.floor(lo / step + 1e-9) * step
    b = math.ceil(hi / step - 1e-9) * step
    ticks, v = [], a
    while v <= b + step * 1e-9:
        ticks.append(round(v, 10))
        v += step
    return ticks, a, b


def log_ticks(lo, hi):
    a = math.floor(math.log10(lo))
    b = math.ceil(math.log10(hi))
    ticks = [10.0 ** e for e in range(a, b + 1)]
    return ticks, 10.0 ** a, 10.0 ** b


def fmt_tick(v):
    if v == 0:
        return "0"
    if abs(v) >= 1000 or (abs(v) < 0.01):
        s = f"{v:g}"
    else:
        s = f"{v:.10g}"
    return s


class Svg:
    def __init__(self):
        self.parts = []

    def add(self, s):
        self.parts.append(s)

    def text(self, x, y, s, cls="ink", anchor="start", extra=""):
        self.add(f'<text x="{x:.1f}" y="{y:.1f}" class="{cls}" text-anchor="{anchor}"{extra}>{esc(s)}</text>')


def legend(svg, names, y, left=16):
    """Wrapped legend; returns the y after it."""
    x = left
    for i, n in enumerate(names):
        w = 22 + len(n) * CHAR + 14
        if x + w > W - 8 and x > left:
            x = left
            y += 20
        svg.add(f'<rect x="{x}" y="{y - 10}" width="12" height="12" rx="2" fill="{PALETTE[i % len(PALETTE)]}"/>')
        svg.text(x + 18, y + 1, n, "ink")
        x += w
    return y + 22


def header(svg, spec):
    y = 24
    for ln in wrap(spec["title"], W - 32):
        svg.text(16, y, ln, "ink t")
        y += 18
    return y


def finish(svg, spec, height):
    alt = spec["alt"]
    body = "\n".join(svg.parts)
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{height:.0f}" '
            f'viewBox="0 0 {W} {height:.0f}" role="img" aria-labelledby="t d">\n'
            f'<title id="t">{esc(spec["title"])}</title><desc id="d">{esc(alt)}</desc>\n{STYLE}\n'
            f'<rect class="bg" x="0.5" y="0.5" width="{W - 1}" height="{height - 1:.0f}" rx="8"/>\n{body}\n</svg>\n')


def vrange(vals, log, zero=True):
    vs = [v for v in vals if v is not None]
    if log:
        lo, hi = min(v for v in vs if v > 0), max(vs)
        return log_ticks(lo, hi)
    lo, hi = min(vs), max(vs)
    if zero:
        lo, hi = min(lo, 0), max(hi, 0)
    return nice_ticks(lo, hi)


def scale(v, a, b, p0, p1, log):
    if log:
        v = max(v, a)
        t = (math.log10(v) - math.log10(a)) / (math.log10(b) - math.log10(a))
    else:
        t = (v - a) / (b - a)
    return p0 + t * (p1 - p0)


def label_fmt(v, dp):
    return f"{v:.{dp}f}"


def render_hbars(spec):
    cats, series = spec["categories"], spec["series"]
    dp, log = spec.get("dp", 2), spec.get("log", False)
    ns = len(series)
    svg = Svg()
    y = header(svg, spec)
    if ns > 1:
        y = legend(svg, [s["name"] for s in series], y + 6)
    else:
        y += 6
    lab_w = 150
    lines = [wrap(c, lab_w - 8) for c in cats]
    ticks, a, b = vrange([v for s in series for v in s["values"]], log)
    x0, x1 = 16 + lab_w, W - 52
    barh = 14 if ns > 1 else 18
    top = y
    for i, c in enumerate(cats):
        rh = max(ns * (barh + 2) + 8, len(lines[i]) * 15 + 6)
        mid = y + rh / 2
        ly = mid - (len(lines[i]) - 1) * 7.5 + 4
        for k, ln in enumerate(lines[i]):
            svg.text(x0 - 8, ly + k * 15, ln, "ink", "end")
        for j, s in enumerate(series):
            v = s["values"][i]
            if v is None:
                continue
            by = mid - ns * (barh + 2) / 2 + j * (barh + 2) + 1
            xa, xv = scale(max(a, 0) if not log else a, a, b, x0, x1, log), scale(v, a, b, x0, x1, log)
            if log:
                xa = x0
            lo_x, hi_x = min(xa, xv), max(xa, xv)
            svg.add(f'<rect x="{lo_x:.1f}" y="{by:.1f}" width="{max(hi_x - lo_x, 1):.1f}" height="{barh}" '
                    f'fill="{PALETTE[j % len(PALETTE)]}"/>')
            if spec.get("labels", True):
                tx = hi_x + 4 if v >= 0 else lo_x - 4
                svg.text(tx, by + barh - 2, label_fmt(v, dp), "sub", "start" if v >= 0 else "end",
                         ' style="font-size:11px"')
        y += rh
    bot = y
    for t in ticks:
        x = scale(t, a, b, x0, x1, log)
        svg.add(f'<line class="grid" x1="{x:.1f}" x2="{x:.1f}" y1="{top - 2}" y2="{bot}"/>')
        svg.text(x, bot + 16, fmt_tick(t), "sub", "middle", ' style="font-size:11px"')
    if not log and a < 0 < b:
        x = scale(0, a, b, x0, x1, log)
        svg.add(f'<line class="axis" x1="{x:.1f}" x2="{x:.1f}" y1="{top - 2}" y2="{bot}"/>')
    else:
        svg.add(f'<line class="axis" x1="{x0}" x2="{x0}" y1="{top - 2}" y2="{bot}"/>')
    yy = bot + 34
    for ln in wrap(spec["value_axis"] + (", log scale" if log else ""), W - 32):
        svg.text(x0 + (x1 - x0) / 2, yy, ln, "sub", "middle")
        yy += 16
    return finish(svg, spec, yy + 6)


def render_bars(spec):
    cats, series = spec["categories"], spec["series"]
    dp, log = spec.get("dp", 2), spec.get("log", False)
    ns = len(series)
    svg = Svg()
    y = header(svg, spec)
    y = legend(svg, [s["name"] for s in series], y + 6) if ns > 1 else y + 6
    ticks, a, b = vrange([v for s in series for v in s["values"]], log)
    x0, x1 = 58, W - 12
    ph = 190
    # category labels first, to size the bottom margin
    gw = (x1 - x0) / len(cats)
    wl = [wrap(c, gw - 4) for c in cats]
    nl = max(len(w) for w in wl)
    top, bot = y + 8, y + 8 + ph
    for t in ticks:
        yy = scale(t, a, b, bot, top, log)
        svg.add(f'<line class="grid" x1="{x0}" x2="{x1}" y1="{yy:.1f}" y2="{yy:.1f}"/>')
        svg.text(x0 - 6, yy + 4, fmt_tick(t), "sub", "end", ' style="font-size:11px"')
    base = scale(0, a, b, bot, top, False) if not log else bot
    bw = min(34.0, (gw - 8) / ns)
    for i, c in enumerate(cats):
        gx = x0 + i * gw + gw / 2 - ns * bw / 2
        for j, s in enumerate(series):
            v = s["values"][i]
            if v is None:
                continue
            yv = scale(v, a, b, bot, top, log)
            ya, yb = min(base, yv), max(base, yv)
            svg.add(f'<rect x="{gx + j * bw:.1f}" y="{ya:.1f}" width="{bw - 1:.1f}" height="{max(yb - ya, 1):.1f}" '
                    f'fill="{PALETTE[j % len(PALETTE)]}"/>')
            if spec.get("labels", True) and ns * len(cats) <= 16:
                ty = ya - 3 if v >= 0 else yb + 11
                svg.text(gx + j * bw + (bw - 1) / 2, ty, label_fmt(v, dp), "sub", "middle", ' style="font-size:11px"')
        for k, ln in enumerate(wl[i]):
            svg.text(x0 + i * gw + gw / 2, bot + 16 + k * 15, ln, "ink", "middle")
    svg.add(f'<line class="axis" x1="{x0}" x2="{x1}" y1="{base:.1f}" y2="{base:.1f}"/>')
    svg.add(f'<line class="axis" x1="{x0}" x2="{x0}" y1="{top}" y2="{bot}"/>')
    yy = bot + 16 + nl * 15 + 10
    lab = spec["value_axis"] + (", log scale" if log else "")
    for ln in wrap(lab, W - 32):
        svg.text(W / 2, yy, ln, "sub", "middle")
        yy += 16
    return finish(svg, spec, yy + 6)


def render_dumbbell(spec):
    cats, series = spec["categories"], spec["series"]
    assert len(series) == 2, "dumbbell needs two series: before, after"
    dp, log = spec.get("dp", 2), spec.get("log", False)
    svg = Svg()
    y = legend(svg, [s["name"] for s in series], header(svg, spec) + 6)
    lab_w = 150
    lines = [wrap(c, lab_w - 8) for c in cats]
    ticks, a, b = vrange([v for s in series for v in s["values"]], log)
    x0, x1 = 16 + lab_w, W - 20
    top = y
    for i, c in enumerate(cats):
        rh = max(30, len(lines[i]) * 15 + 6)
        mid = y + rh / 2
        ly = mid - (len(lines[i]) - 1) * 7.5 + 4
        for k, ln in enumerate(lines[i]):
            svg.text(x0 - 8, ly + k * 15, ln, "ink", "end")
        v0, v1 = series[0]["values"][i], series[1]["values"][i]
        xa, xb = scale(v0, a, b, x0, x1, log), scale(v1, a, b, x0, x1, log)
        svg.add(f'<line x1="{xa:.1f}" x2="{xb:.1f}" y1="{mid:.1f}" y2="{mid:.1f}" stroke="#999999" stroke-width="3"/>')
        svg.add(f'<circle cx="{xa:.1f}" cy="{mid:.1f}" r="6" fill="{PALETTE[0]}"/>')
        svg.add(f'<rect x="{xb - 6:.1f}" y="{mid - 6:.1f}" width="12" height="12" fill="{PALETTE[1]}"/>')
        if spec.get("labels", True):
            right = xb >= xa
            svg.text(xb + (10 if right else -10), mid + 4, label_fmt(v1, dp), "sub", "start" if right else "end",
                     ' style="font-size:11px"')
            svg.text(xa + (-10 if right else 10), mid + 4, label_fmt(v0, dp), "sub", "end" if right else "start",
                     ' style="font-size:11px"')
        y += rh
    bot = y
    for t in ticks:
        x = scale(t, a, b, x0, x1, log)
        svg.add(f'<line class="grid" x1="{x:.1f}" x2="{x:.1f}" y1="{top - 2}" y2="{bot}"/>')
        svg.text(x, bot + 16, fmt_tick(t), "sub", "middle", ' style="font-size:11px"')
    yy = bot + 34
    for ln in wrap(spec["value_axis"] + (", log scale" if log else ""), W - 32):
        svg.text(x0 + (x1 - x0) / 2, yy, ln, "sub", "middle")
        yy += 16
    return finish(svg, spec, yy + 6)


MARKERS = ["circle", "square", "diamond", "triangle"]


def marker(kind, x, y, colour, r=4.5):
    if kind == "circle":
        return f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r}" fill="{colour}"/>'
    if kind == "square":
        return f'<rect x="{x - r:.1f}" y="{y - r:.1f}" width="{2 * r}" height="{2 * r}" fill="{colour}"/>'
    if kind == "diamond":
        return (f'<polygon points="{x:.1f},{y - r - 1:.1f} {x + r + 1:.1f},{y:.1f} {x:.1f},{y + r + 1:.1f} '
                f'{x - r - 1:.1f},{y:.1f}" fill="{colour}"/>')
    return (f'<polygon points="{x:.1f},{y - r - 1:.1f} {x + r + 1:.1f},{y + r:.1f} {x - r - 1:.1f},{y + r:.1f}" '
            f'fill="{colour}"/>')


def render_xy(spec, lines):
    series = spec["series"]
    log, logx = spec.get("log", False), spec.get("logx", False)
    svg = Svg()
    y = header(svg, spec)
    if len(series) > 1:
        y = legend(svg, [s["name"] for s in series], y + 6)
    else:
        y += 6
    pts = spec.get("points")
    xs = [x for s in series for x in s.get("x", [])] + [p["x"] for p in (pts or [])]
    ys = [v for s in series for v in s.get("values", []) if v is not None] + [p["y"] for p in (pts or [])]
    xt, xa, xb = vrange(xs, logx, zero=False) if xs else ([0, 1], 0, 1)
    yt, ya, yb = vrange(ys, log, zero=not log and min(ys) >= 0 and min(ys) < 0.5 * max(ys))
    x0, x1 = 58, W - 18
    top, bot = y + 8, y + 8 + 210
    if spec.get("equal_axes"):
        both = xs + ys
        lo_hi = [min(both), max(both)]
        if logx != log:
            raise SystemExit("equal_axes needs log and logx to agree")
        xt, xa, xb = vrange(lo_hi, logx, zero=False)
        yt, ya, yb = xt, xa, xb
        bot = top + min(x1 - x0, 320)
    for t in yt:
        yy = scale(t, ya, yb, bot, top, log)
        svg.add(f'<line class="grid" x1="{x0}" x2="{x1}" y1="{yy:.1f}" y2="{yy:.1f}"/>')
        svg.text(x0 - 6, yy + 4, fmt_tick(t), "sub", "end", ' style="font-size:11px"')
    for t in xt:
        xx = scale(t, xa, xb, x0, x1, logx)
        svg.add(f'<line class="grid" x1="{xx:.1f}" x2="{xx:.1f}" y1="{top}" y2="{bot}"/>')
        svg.text(xx, bot + 16, fmt_tick(t), "sub", "middle", ' style="font-size:11px"')
    svg.add(f'<line class="axis" x1="{x0}" x2="{x1}" y1="{bot}" y2="{bot}"/>')
    svg.add(f'<line class="axis" x1="{x0}" x2="{x0}" y1="{top}" y2="{bot}"/>')
    if spec.get("identity_line"):
        lo_, hi_ = max(xa, ya), min(xb, yb)
        svg.add(f'<line x1="{scale(lo_, xa, xb, x0, x1, logx):.1f}" y1="{scale(lo_, ya, yb, bot, top, log):.1f}" '
                f'x2="{scale(hi_, xa, xb, x0, x1, logx):.1f}" y2="{scale(hi_, ya, yb, bot, top, log):.1f}" '
                f'stroke="#999999" stroke-width="1.5" stroke-dasharray="5 4"/>')
    for j, s in enumerate(series):
        col = PALETTE[j % len(PALETTE)]
        pp = [(scale(x, xa, xb, x0, x1, logx), scale(v, ya, yb, bot, top, log))
              for x, v in zip(s.get("x", []), s.get("values", [])) if v is not None]
        if lines and len(pp) > 1:
            d = " ".join(("M" if k == 0 else "L") + f"{px:.1f},{py:.1f}" for k, (px, py) in enumerate(pp))
            svg.add(f'<path class="ln" d="{d}" stroke="{col}" stroke-width="2"/>')
        for px, py in pp:
            svg.add(marker(MARKERS[j % 4], px, py, col))
    if pts:
        for p in pts:
            px, py = scale(p["x"], xa, xb, x0, x1, logx), scale(p["y"], ya, yb, bot, top, log)
            svg.add(marker("circle", px, py, PALETTE[0], 5))
            if p.get("label"):
                right = px < (x0 + x1) / 2
                svg.text(px + (8 if right else -8), py - 7, p["label"], "ink", "start" if right else "end",
                         ' style="font-size:11px"')
    yy = bot + 36
    for ln in wrap(spec.get("xlabel", ""), W - 32):
        svg.text((x0 + x1) / 2, yy, ln, "sub", "middle")
        yy += 16
    # y label as a line under the x label (keeps the picture narrow and the text horizontal)
    lab = "Vertical: " + spec["ylabel"] + (", log scale" if log else "")
    for ln in wrap(lab, W - 32):
        svg.text((x0 + x1) / 2, yy, ln, "sub", "middle")
        yy += 16
    return finish(svg, spec, yy + 6)


RENDER = {"bars": render_bars, "hbars": render_hbars, "dumbbell": render_dumbbell,
          "lines": lambda s: render_xy(s, True), "scatter": lambda s: render_xy(s, False)}


def numbers_in(text):
    return {float(m) for m in re.findall(r"-?\d+(?:\.\d+)?", text.replace(",", ""))} | \
           {abs(float(m)) for m in re.findall(r"-?\d+(?:\.\d+)?", text.replace(",", ""))}


def spec_numbers(spec):
    out = []
    for s in spec.get("series", []):
        out += [v for v in s.get("values", []) if v is not None]
        out += list(s.get("x", []))
    for p in spec.get("points", []):
        out += [p["x"], p["y"]]
    return out


def validate(spec, src, page_cache):
    errs = []
    for k in ("id", "page", "kind", "title", "caption", "alt"):
        if not spec.get(k):
            errs.append(f"missing {k}")
    if spec.get("kind") not in RENDER:
        errs.append(f"unknown kind {spec.get('kind')}")
        return errs
    if spec["kind"] in ("bars", "hbars", "dumbbell"):
        if "categories" not in spec or "value_axis" not in spec:
            errs.append("needs categories and value_axis")
        else:
            for s in spec["series"]:
                if len(s["values"]) != len(spec["categories"]):
                    errs.append(f"series {s['name']}: {len(s['values'])} values for {len(spec['categories'])} categories")
    elif "ylabel" not in spec or "xlabel" not in spec:
        errs.append("needs xlabel and ylabel")
    page = src / spec["page"]
    if not page.exists():
        return errs + [f"page {spec['page']} not found"]
    text = page_cache.setdefault(page, page.read_text(encoding="utf8"))
    have = numbers_in(text)
    for v in spec_numbers(spec):
        if float(v) not in have:
            errs.append(f"number {v} is not on {spec['page']}")
    img = f"images/charts/{spec['id']}.svg"
    if img not in text:
        errs.append(f"{spec['page']} does not show {img}")
    if spec["caption"].strip()[:30] not in text:
        errs.append(f"{spec['page']} does not carry the caption")
    return errs


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--check", action="store_true", help="write nothing; fail if a chart is stale or unverified")
    ap.add_argument("--root", default=".", help="repository root")
    ap.add_argument("--src", default="docs/book/src", help="book source directory under the root")
    a = ap.parse_args()
    src = Path(a.root) / a.src
    data, out = src / "data" / "charts", src / "images" / "charts"
    specs = sorted(data.glob("*.json"))
    bad = 0
    cache = {}
    names = set()
    for f in specs:
        spec = json.loads(f.read_text(encoding="utf8"))
        spec.setdefault("series", [])
        if spec.get("id") != f.stem:
            print(f"{f.name}: id must equal the file name")
            bad += 1
            continue
        errs = validate(spec, src, cache)
        if errs:
            bad += 1
            for e in errs:
                print(f"{f.name}: {e}")
            continue
        svg = RENDER[spec["kind"]](spec)
        names.add(f"{spec['id']}.svg")
        target = out / f"{spec['id']}.svg"
        if a.check:
            # compare bytes with line endings normalised, so a CRLF checkout does not flap
            if not target.exists() or target.read_bytes().replace(b"\r\n", b"\n") != svg.encode("utf8"):
                print(f"{target.relative_to(a.root)} is stale: run python3 tools/docs-graphs/make_graphs.py")
                bad += 1
        else:
            out.mkdir(parents=True, exist_ok=True)
            target.write_text(svg, encoding="utf8", newline="\n")
    if out.exists():
        for p in sorted(out.glob("*.svg")):
            if p.name not in names:
                print(f"{p.relative_to(a.root)} has no data file")
                bad += 1
    print(f"{len(names)} charts, {bad} problem(s)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
