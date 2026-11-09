"""Convert the website font to the welcome screen's Unicode wordmark.

Development-only dependency: Pillow. The application uses the resulting Rust
strings, with no image renderer, font or Python dependency.
"""
import argparse
import json
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


def blocks(mask, columns, threshold):
    # A terminal cell is approximately twice as tall as it is wide. Quadrant
    # glyphs give two horizontal and two vertical samples per cell.
    height = round(mask.height / mask.width * columns)
    mask = mask.resize((columns * 2, height), Image.Resampling.LANCZOS)
    alphabet = " ▘▝▀▖▌▞▛▗▚▐▜▄▙▟█"
    rows = []
    for y in range(0, height, 2):
        row = ""
        for x in range(0, columns * 2, 2):
            bits = sum(1 << (dy * 2 + dx)
                       for dy in range(2) for dx in range(2)
                       if y + dy < height and mask.getpixel((x + dx, y + dy)) >= threshold)
            row += alphabet[bits]
        rows.append(row.rstrip())
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--font", type=Path, required=True, help="Website Space Grotesk 600 font")
    parser.add_argument("--output", type=Path, default=Path("src/branding.rs"))
    args = parser.parse_args()
    font = ImageFont.truetype(str(args.font), 256)
    left, top, right, bottom = font.getbbox("roomkit.")
    lettering = Image.new("L", (right - left, bottom - top))
    ImageDraw.Draw(lettering).text((-left, -top), "roomkit.", font=font, fill=255)
    text = "//! Generated from Space Grotesk 600.\n//! Regenerate with scripts/generate-branding.py; layout belongs to roomkit-tui.\n"
    text += "\npub(crate) fn welcome_wordmark() -> Vec<String> {\n    [\n"
    text += "".join(f"        {json.dumps(row, ensure_ascii=False)},\n" for row in blocks(lettering, 52, 110))
    text += "    ]\n    .into_iter()\n    .map(String::from)\n    .collect()\n}\n"
    args.output.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    main()
