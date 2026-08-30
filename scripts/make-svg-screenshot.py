#!/usr/bin/env python3
"""Render ByteAI terminal output as GitHub-renderable SVG screenshots.

Takes a text file (captured terminal output) and wraps it in a terminal
window chrome, producing an .svg that GitHub renders inline in the README.
"""
import html
import sys

def render_svg(title: str, lines: list[str], out: str, width: int = 92):
    char_w = 8.2
    line_h = 18
    pad = 14
    header_h = 34
    h = header_h + pad * 2 + len(lines) * line_h
    w = pad * 2 + width * char_w

    parts = []
    parts.append(f'<svg xmlns="http://www.w3.org/2000/svg" width="{int(w)}" height="{int(h)}" viewBox="0 0 {int(w)} {int(h)}">')
    parts.append('<defs>')
    parts.append('<linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">')
    parts.append('<stop offset="0%" stop-color="#1e1f26"/>')
    parts.append('<stop offset="100%" stop-color="#141519"/>')
    parts.append('</linearGradient>')
    parts.append('</defs>')
    # window bg
    parts.append(f'<rect x="0" y="0" width="{int(w)}" height="{int(h)}" rx="10" fill="url(#bg)"/>')
    # title bar
    parts.append(f'<rect x="0" y="0" width="{int(w)}" height="{header_h}" rx="10" fill="#26272e"/>')
    parts.append(f'<rect x="0" y="{header_h - 10}" width="{int(w)}" height="10" fill="#26272e"/>')
    # traffic lights
    for cx in (18, 36, 54):
        parts.append(f'<circle cx="{cx}" cy="17" r="5.5" fill="#ff5f57"/>')
    parts.append(f'<circle cx="36" cy="17" r="5.5" fill="#febc2e"/>')
    parts.append(f'<circle cx="54" cy="17" r="5.5" fill="#28c840"/>')
    # title
    parts.append(f'<text x="{int(w/2)}" y="22" fill="#9a9ba5" font-family="Menlo,Consolas,monospace" font-size="12" text-anchor="middle">{html.escape(title)}</text>')
    # content
    y = header_h + pad + line_h - 4
    for line in lines:
        parts.append(f'<text x="{pad}" y="{y}" fill="#d6d7de" font-family="Menlo,Consolas,monospace" font-size="13">{html.escape(line)}</text>')
        y += line_h
    parts.append('</svg>')
    with open(out, 'w') as f:
        f.write('\n'.join(parts))
    print(f"wrote {out} ({len(lines)} lines)")

if __name__ == '__main__':
    title = sys.argv[1]
    infile = sys.argv[2]
    outfile = sys.argv[3]
    with open(infile) as f:
        lines = [l.rstrip('\n') for l in f.readlines()]
    render_svg(title, lines, outfile)
