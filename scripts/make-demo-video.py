#!/usr/bin/env python3
"""Generate an animated SVG terminal demo for the ByteAI README.

Renders a "typing" terminal session using SMIL animations, which GitHub
renders inline (plays automatically in the README). This acts as the
project's demo video without requiring a video host.
"""
import html

def esc(s: str) -> str:
    return html.escape(s)

def build(lines: list[str], title: str = "ByteAi — autonomous coding agent") -> str:
    char_w = 8.6
    line_h = 20
    pad = 16
    header_h = 38
    width = 880
    # Each line appears sequentially; hold the last frame.
    n = len(lines)
    per = 1000.0 / n  # ms per line reveal
    total = int(per * n + 3000)
    h = header_h + pad * 2 + n * line_h + 10

    parts = []
    parts.append(f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{h}" viewBox="0 0 {width} {h}">')
    parts.append('<defs>')
    parts.append('<linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">')
    parts.append('<stop offset="0%" stop-color="#15161c"/>')
    parts.append('<stop offset="100%" stop-color="#0d0e12"/>')
    parts.append('</linearGradient>')
    parts.append('<linearGradient id="accent" x1="0" y1="0" x2="1" y2="0">')
    parts.append('<stop offset="0%" stop-color="#6ee7b7"/>')
    parts.append('<stop offset="100%" stop-color="#38bdf8"/>')
    parts.append('</linearGradient>')
    parts.append('</defs>')
    parts.append(f'<rect x="0" y="0" width="{width}" height="{h}" rx="12" fill="url(#bg)"/>')
    parts.append(f'<rect x="0" y="0" width="{width}" height="{header_h}" rx="12" fill="#1c1d24"/>')
    parts.append(f'<rect x="0" y="{header_h - 8}" width="{width}" height="8" fill="#1c1d24"/>')
    for cx in (20, 40, 60):
        parts.append(f'<circle cx="{cx}" cy="19" r="6" fill="#ff5f57"/>')
    parts.append('<circle cx="40" cy="19" r="6" fill="#febc2e"/>')
    parts.append('<circle cx="60" cy="19" r="6" fill="#28c840"/>')
    parts.append(f'<text x="{width // 2}" y="24" fill="#8b8d98" font-family="Menlo,Consolas,monospace" font-size="12.5" text-anchor="middle">{esc(title)}</text>')
    # accent underline animating across the header
    parts.append(f'<rect x="0" y="{header_h - 3}" width="0" height="3" fill="url(#accent)">')
    parts.append(f'<animate attributeName="width" from="0" to="{width}" dur="{(total - 1500)/1000:.1f}s" fill="freeze"/>')
    parts.append('</rect>')

    # Prompt cursor line
    prompt_y = header_h + pad + line_h - 2
    parts.append(f'<text x="{pad}" y="{prompt_y}" fill="#6ee7b7" font-family="Menlo,Consolas,monospace" font-size="14">❯</text>')
    parts.append(f'<text x="{pad + 22}" y="{prompt_y}" fill="#d6d7de" font-family="Menlo,Consolas,monospace" font-size="14">byteai</text>')
    parts.append(f'<rect x="{pad + 96}" y="{prompt_y - 12}" width="9" height="16" fill="#38bdf8">')
    parts.append('<animate attributeName="opacity" values="1;0;1" dur="1.1s" repeatCount="indefinite"/>')
    parts.append('</rect>')

    # Output lines revealed sequentially
    y = prompt_y + line_h
    for i, line in enumerate(lines):
        # blank content behind (so layout is stable)
        parts.append(f'<text x="{pad}" y="{y}" fill="#d6d7de" font-family="Menlo,Consolas,monospace" font-size="13.5" opacity="0">{esc(line)}</text>')
        # colored echo: lines starting with special prefixes
        fill = "#d6d7de"
        if line.startswith("[") :
            fill = "#9aa0b4"
        elif line.startswith("/") or line.startswith("❯") or line.startswith("byteai"):
            fill = "#6ee7b7"
        elif line.startswith("✓"):
            fill = "#6ee7b7"
        elif line.startswith("──"):
            fill = "#38bdf8"
        parts.append(f'<text x="{pad}" y="{y}" fill="{fill}" font-family="Menlo,Consolas,monospace" font-size="13.5" opacity="0">')
        parts.append(f'<animate attributeName="opacity" from="0" to="1" begin="{(i * per)/1000:.2f}s" dur="0.35s" fill="freeze"/>')
        parts.append(f'</text>')
        y += line_h

    # blinking cursor at the end
    parts.append(f'<rect x="{pad}" y="{y - 10}" width="9" height="16" fill="#6ee7b7" opacity="0">')
    parts.append(f'<animate attributeName="opacity" values="0;1;0" begin="{(n * per)/1000:.2f}s" dur="1.2s" repeatCount="indefinite"/>')
    parts.append('</rect>')
    parts.append('</svg>')
    return "\n".join(parts)

if __name__ == "__main__":
    import sys
    lines = [l.rstrip("\n") for l in sys.stdin.readlines()]
    print(build(lines))
