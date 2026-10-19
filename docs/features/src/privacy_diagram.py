"""Writes docs/features/privacy-dark.svg and privacy-light.svg.

Run from the repository root: python docs/features/src/privacy_diagram.py
Colours follow phorminx.net (Abyss, Limestone, Bronze).
"""
from pathlib import Path

THEMES = {
    "dark": dict(bg="#0c0f11", panel="#15191c", border="#343a3d", text="#eceae4",
                 muted="#aeb4b7", local="#7fa88a", remote="#c69767", line="#62666a"),
    "light": dict(bg="#f2f0e9", panel="#faf8f2", border="#cbc7bd", text="#1f2022",
                  muted="#62666a", local="#4f7a5a", remote="#a87542", line="#8c8a84"),
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
        'font-family="Segoe UI Variable,Segoe UI,-apple-system,BlinkMacSystemFont,Helvetica,Arial,sans-serif" role="img" '
        'aria-label="What stays on your computer and what can leave it">',
        f'<rect width="{width}" height="{height}" rx="0" fill="{c["bg"]}"/>',
    ]
    columns = [(24, "Stays on this computer", c["local"], LOCAL),
               (446, "Leaves only when you choose", c["remote"], REMOTE)]
    for x, title, accent, items in columns:
        out.append(f'<rect x="{x}" y="20" width="350" height="{height - 40}" rx="0" '
                   f'fill="{c["panel"]}" stroke="{c["border"]}"/>')
        out.append(f'<rect x="{x + 18}" y="44" width="12" height="12" fill="{accent}"/>')
        out.append(f'<text x="{x + 40}" y="56" font-size="13" font-weight="650" letter-spacing="2" fill="{accent}">{escape(title.upper())}</text>')
        for index, (name, detail) in enumerate(items):
            y = top + index * row
            out.append(f'<rect x="{x + 16}" y="{y - 6}" width="318" height="{row - 10}" rx="0" '
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
