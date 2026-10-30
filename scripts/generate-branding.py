"""Convert the RoomKit SVG and website font to embedded Unicode terminal art.

Development-only dependencies: CairoSVG and Pillow. The application uses the
resulting Rust strings, with no image renderer, font or Python dependency.
"""
import argparse
import io
import json
from pathlib import Path
import xml.etree.ElementTree as ET

import cairosvg
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
    parser.add_argument("--logo", type=Path, default=Path("assets/roomkit-mark.svg"))
    parser.add_argument("--font", type=Path, required=True, help="Website Space Grotesk 600 font")
    parser.add_argument("--output", type=Path, default=Path("src/branding.rs"))
    args = parser.parse_args()
    root = ET.parse(args.logo).getroot()
    # Monochrome artwork inherits the host theme; preserve shape, flatten alpha.
    for element in root.iter():
        if "opacity" in element.attrib:
            element.set("opacity", "1")
    png = cairosvg.svg2png(bytestring=ET.tostring(root), output_width=256, output_height=256)
    symbol = Image.open(io.BytesIO(png)).convert("RGBA").getchannel("A")
    # Hint the thin outline for the compact grid so it survives downsampling.
    # The SVG still owns all geometry; the welcome artwork keeps its full detail.
    for element in root.iter():
        if "stroke-width" in element.attrib:
            element.set("stroke-width", str(float(element.get("stroke-width")) * 1.5))
    png = cairosvg.svg2png(bytestring=ET.tostring(root), output_width=256, output_height=256)
    compact = Image.open(io.BytesIO(png)).convert("RGBA").getchannel("A")
    font = ImageFont.truetype(str(args.font), 256)
    left, top, right, bottom = font.getbbox("roomkit.")
    lettering = Image.new("L", (right - left, bottom - top))
    ImageDraw.Draw(lettering).text((-left, -top), "roomkit.", font=font, fill=255)
    text = "//! Generated from the RoomKit SVG and Space Grotesk 600.\n//! Regenerate with scripts/generate-branding.py; layout belongs to roomkit-tui.\n"
    # Seven vertical samples retain a gap between the four cells in four rows.
    for name, rows in [("header_logo", blocks(compact, 7, 140)),
                       ("welcome_logo", blocks(symbol, 14, 128)),
                       ("welcome_wordmark", blocks(lettering, 52, 110))]:
        text += f"\npub(crate) fn {name}() -> Vec<String> {{\n    [\n"
        text += "".join(f"        {json.dumps(row, ensure_ascii=False)},\n" for row in rows)
        text += "    ]\n    .into_iter()\n    .map(String::from)\n    .collect()\n}\n"
    args.output.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    main()
