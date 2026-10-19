"""Writes docs/features/privacy-dark.svg and privacy-light.svg.

Run from the repository root: python docs/features/capture/privacy_diagram.py
"""
from pathlib import Path

THEMES = {
    "dark": dict(bg="#0d1117", panel="#161b22", border="#30363d", text="#e6edf3",
                 muted="#8b949e", local="#3fb950", remote="#d29922", line="#6e7681"),
    "light": dict(bg="#ffffff", panel="#f6f8fa", border="#d0d7de", text="#1f2328",
                  muted="#59636e", local="#1a7f37", remote="#9a6700", line="#8c959f"),
}

LOCAL = [
    ("Speech recognition", "Vosk and Whisper; audio stays in memory"),
    ("Formatting and translation", "local Ollama models"),
    ("Semantic search", "local embeddings"),
    ("Screenshot text (OCR)", "local models, verified download"),
    ("Your text", "history, meetings, investigations, prompts"),
]
REMOTE = [
    ("Feedback", "only after you press Send, to phorminx.net"),
    ("Remote assistant", "OpenAI or Anthropic, if you pick one"),
    ("Actions", "your saved HTTP request, when you run it"),
    ("Messages", "off by default; direct to contacts over Tor"),
    ("Downloads and updates", "models, link previews, GitHub Releases"),
]


def escape(value: str) -> str:
    return value.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def render(c: dict) -> str:
    width, row, top = 820, 58, 92
    height = top + row * len(LOCAL) + 28
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" height="{height}" '
        'font-family="-apple-system,BlinkMacSystemFont,Segoe UI,Helvetica,Arial,sans-serif" role="img" '
        'aria-label="What stays on your computer and what can leave it">',
        f'<rect width="{width}" height="{height}" rx="12" fill="{c["bg"]}"/>',
    ]
    columns = [(24, "Stays on this computer", c["local"], LOCAL),
               (446, "Leaves only when you choose", c["remote"], REMOTE)]
    for x, title, accent, items in columns:
        out.append(f'<rect x="{x}" y="20" width="350" height="{height - 40}" rx="10" '
                   f'fill="{c["panel"]}" stroke="{c["border"]}"/>')
        out.append(f'<circle cx="{x + 24}" cy="50" r="6" fill="{accent}"/>')
        out.append(f'<text x="{x + 40}" y="56" font-size="17" font-weight="600" fill="{c["text"]}">{escape(title)}</text>')
        for index, (name, detail) in enumerate(items):
            y = top + index * row
            out.append(f'<rect x="{x + 16}" y="{y - 6}" width="318" height="{row - 10}" rx="7" '
                       f'fill="{c["bg"]}" stroke="{c["border"]}"/>')
            out.append(f'<rect x="{x + 16}" y="{y - 6}" width="4" height="{row - 10}" rx="2" fill="{accent}"/>')
            out.append(f'<text x="{x + 32}" y="{y + 14}" font-size="14" font-weight="600" fill="{c["text"]}">{escape(name)}</text>')
            out.append(f'<text x="{x + 32}" y="{y + 33}" font-size="12.5" fill="{c["muted"]}">{escape(detail)}</text>')
    y = height // 2 + 10
    out.append(f'<path d="M 382 {y} L 436 {y}" stroke="{c["line"]}" stroke-width="2" '
               f'stroke-dasharray="5 4" fill="none"/>')
    out.append(f'<path d="M 429 {y - 6} L 438 {y} L 429 {y + 6}" stroke="{c["line"]}" stroke-width="2" fill="none"/>')
    out.append("</svg>")
    return "\n".join(out) + "\n"


if __name__ == "__main__":
    target = Path("docs/features")
    for theme, colours in THEMES.items():
        (target / f"privacy-{theme}.svg").write_text(render(colours), encoding="utf-8")
        print(f"wrote privacy-{theme}.svg")
