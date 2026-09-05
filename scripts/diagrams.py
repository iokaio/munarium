#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build the documentation diagrams as committed SVG.

The diagrams used to be PNGs with no source of any kind -- not an SVG, not a
drawio, not a mermaid fence. That made them unreviewable (a diff showed only
that bytes changed), unmaintainable (a stale count could only be fixed by
whoever still had the original tool), and unscannable: the longest-surviving
private measurement in this repository lived inside `ch19-patterns-map.png`,
where no text search could reach it.

So the diagrams are text now. This module is the renderer; the specs live
beside it in `server.py` and `matrix.py`, one dict per figure. Regenerate with:

    py scripts/diagrams.py

Output is plain SVG with no external font, no script and no embedded raster,
and it reads on a light or a dark background because every colour is drawn
from a `currentColor`-independent palette with an explicit background rect.
"""
from __future__ import annotations

import html
import pathlib
import sys

# A restrained palette: one neutral, one accent, one "watch out". Deliberately
# not a rainbow -- a diagram that needs six hues to be legible is a diagram
# that is doing too much.
PALETTE = {
    "ink": "#1b2733",
    "muted": "#5a6b7d",
    "line": "#93a3b4",
    "paper": "#fbfcfd",
    "band": "#eef2f6",
    "accent": "#1f5c8b",
    "accentfill": "#e3edf5",
    "warn": "#a8442a",
    "warnfill": "#f6e5e0",
    "ok": "#2c6e54",
    "okfill": "#e2efe9",
}

FONT = ("ui-sans-serif, -apple-system, 'Segoe UI', Roboto, "
        "'Helvetica Neue', Arial, sans-serif")
MONO = ("ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, "
        "'Liberation Mono', monospace")


def esc(s: str) -> str:
    return html.escape(str(s), quote=True)


def wrap(text: str, width: int) -> list[str]:
    """Greedy wrap on spaces; the specs keep labels short enough that this is
    all the typography a box diagram needs."""
    out: list[str] = []
    for para in text.split("\n"):
        line = ""
        for word in para.split():
            trial = f"{line} {word}".strip()
            if len(trial) <= width:
                line = trial
            else:
                if line:
                    out.append(line)
                line = word
        out.append(line)
    return out


class Svg:
    def __init__(self, w: int, h: int, title: str):
        self.w, self.h, self.title = w, h, title
        self.parts: list[str] = []

    # -- primitives ---------------------------------------------------------
    def rect(self, x, y, w, h, fill="paper", stroke="line", rx=6, dash=None,
             width=1.4):
        d = f' stroke-dasharray="{dash}"' if dash else ""
        self.parts.append(
            f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" '
            f'fill="{PALETTE.get(fill, fill)}" stroke="{PALETTE.get(stroke, stroke)}" '
            f'stroke-width="{width}"{d}/>')

    def text(self, x, y, s, size=13, fill="ink", anchor="middle", mono=False,
             weight="normal", italic=False):
        fam = MONO if mono else FONT
        st = ' font-style="italic"' if italic else ""
        self.parts.append(
            f'<text x="{x}" y="{y}" font-family="{fam}" font-size="{size}" '
            f'fill="{PALETTE.get(fill, fill)}" text-anchor="{anchor}" '
            f'font-weight="{weight}"{st}>{esc(s)}</text>')

    def lines(self, x, y, rows, size=12, fill="muted", lh=15, anchor="middle",
              mono=False):
        for i, r in enumerate(rows):
            self.text(x, y + i * lh, r, size=size, fill=fill, anchor=anchor,
                      mono=mono)

    def arrow(self, x1, y1, x2, y2, stroke="line", dash=None, label=None,
              label_dy=-6):
        d = f' stroke-dasharray="{dash}"' if dash else ""
        self.parts.append(
            f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" '
            f'stroke="{PALETTE.get(stroke, stroke)}" stroke-width="1.6" '
            f'marker-end="url(#a-{stroke})"{d}/>')
        if label:
            # A label centred on a vertical line lands ON the line. Push it to
            # the side instead, and keep the above-the-line habit for
            # horizontal ones.
            vertical = abs(x2 - x1) < abs(y2 - y1)
            if vertical:
                self.text(x1 + 8, (y1 + y2) / 2 + 4, label, size=11,
                          fill="muted", anchor="start")
            else:
                self.text((x1 + x2) / 2, (y1 + y2) / 2 + label_dy, label,
                          size=11, fill="muted")

    def render(self) -> str:
        defs = "".join(
            f'<marker id="a-{k}" viewBox="0 0 10 10" refX="9" refY="5" '
            f'markerWidth="7" markerHeight="7" orient="auto-start-reverse">'
            f'<path d="M 0 0 L 10 5 L 0 10 z" fill="{v}"/></marker>'
            for k, v in PALETTE.items())
        return (
            f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {self.w} {self.h}" '
            f'width="{self.w}" height="{self.h}" role="img" '
            f'aria-label="{esc(self.title)}">\n'
            f'<title>{esc(self.title)}</title>\n<defs>{defs}</defs>\n'
            f'<rect width="{self.w}" height="{self.h}" fill="{PALETTE["paper"]}"/>\n'
            + "\n".join(self.parts) + "\n</svg>\n")


# -- composite helpers the specs actually use -------------------------------

def node(s: Svg, x, y, w, h, title, body=(), kind="plain", mono_body=False):
    fill = {"plain": "paper", "accent": "accentfill", "warn": "warnfill",
            "ok": "okfill", "band": "band"}[kind]
    stroke = {"plain": "line", "accent": "accent", "warn": "warn",
              "ok": "ok", "band": "line"}[kind]
    s.rect(x, y, w, h, fill=fill, stroke=stroke)
    cx = x + w / 2
    tl = wrap(title, max(12, int(w / 7.6)))
    ty = y + 22
    for i, line in enumerate(tl):
        s.text(cx, ty + i * 16, line, size=13, weight="600", fill="ink")
    if body:
        by = ty + len(tl) * 16 + 6
        rows: list[str] = []
        for b in body:
            rows.extend(wrap(b, max(14, int(w / 6.4))))
        s.lines(cx, by, rows, size=11, lh=14, mono=mono_body)


def caption(s: Svg, text: str):
    s.text(s.w / 2, s.h - 12, text, size=11, fill="muted", italic=True)


def heading(s: Svg, text: str):
    s.text(s.w / 2, 26, text, size=15, weight="700", fill="ink")


def build(spec_fn, out: pathlib.Path) -> None:
    svg = spec_fn()
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(svg.render(), encoding="utf-8", newline="")


def main() -> int:
    root = pathlib.Path(__file__).resolve().parents[1]
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
    import diagrams_server
    import diagrams_matrix

    n = 0
    for mod, base in ((diagrams_server, root / "server/docs/guides/images"),
                      (diagrams_matrix, root / "matrix/docs/guides/technical/images")):
        for name, fn in mod.FIGURES.items():
            build(fn, base / f"{name}.svg")
            n += 1
    print(f"diagrams: {n} SVG figure(s) written")
    return 0


if __name__ == "__main__":
    sys.exit(main())
