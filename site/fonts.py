"""Refresh the self-hosted landing page fonts. Run after page text gains new CJK characters.

Fetches OFL font subsets from Google Fonts once, so visitors' browsers never contact Google.
Fredoka and Nunito keep Google's latin subset; Chiron GoRound TC (English and Traditional
Chinese pages) and Zen Maru Gothic (Japanese page) keep only the characters the pages use.
"""
from __future__ import annotations

import json
from pathlib import Path
import re
import urllib.parse
import urllib.request

SITE = Path(__file__).resolve().parent
FONTS = SITE / "assets/fonts"
AGENT = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"


def fetch(url: str) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": AGENT})
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read()


def characters(*languages: str) -> str:
    text = SITE.joinpath("index.html").read_text(encoding="utf-8")
    for language in languages:
        text += "".join(json.loads(SITE.joinpath(f"strings/{language}.json").read_text(encoding="utf-8")).values())
    text = re.sub(r"<script.*?</script>|<style.*?</style>|<[^>]+>", "", text, flags=re.S)
    return "".join(sorted({char for char in text if ord(char) > 0xFF}))


def faces(query: str) -> list[tuple[str, str, str]]:
    css = fetch("https://fonts.googleapis.com/css2?" + query).decode("utf-8")
    pattern = r"(?:/\* ([\w-]+) \*/\s*)?@font-face \{[^}]*?font-weight: ([\d ]+);[^}]*?url\((https://[^)]+)\)"
    return re.findall(pattern, css)


def save(name: str, url: str) -> None:
    data = fetch(url)
    if not data.startswith(b"wOF2"):
        raise ValueError(f"{name} is not a WOFF2 font")
    FONTS.joinpath(name).write_bytes(data)
    print(f"{name}: {len(data):,} bytes")


def main() -> None:
    FONTS.mkdir(parents=True, exist_ok=True)
    for family, axis, name in (("Fredoka", "wght@300..700", "fredoka-latin"), ("Nunito", "wght@200..1000", "nunito-latin")):
        latin = [url for subset, _, url in faces(f"family={family}:{axis}&display=swap") if subset == "latin"]
        save(f"{name}.woff2", latin[0])
    for family, axis, languages, name in (("Chiron GoRound TC", "wght@400..900", ("en", "zh-TW"), "chiron-goround-tc"),
                                          ("Zen Maru Gothic", "wght@500;700;900", ("ja",), "zen-maru-gothic")):
        query = urllib.parse.urlencode({"family": f"{family}:{axis}", "text": characters(*languages), "display": "swap"})
        found = faces(query)
        if len(found) == 1:
            save(f"{name}.woff2", found[0][2])
        else:
            for _, weight, url in found:
                save(f"{name}-{weight}.woff2", url)


if __name__ == "__main__":
    main()
