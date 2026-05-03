#!/usr/bin/env python3
"""Reshape `src-tauri/icons/icon.png` to follow the macOS app-icon template:
- 1024x1024 canvas
- Continuous-corner squircle mask at the standard ~22.5% corner radius
  (we use a high-radius rounded rect, the closest practical approximation
  of Apple's superellipse without a full bezier path)
- Content inset so the chevron/bar artwork doesn't crowd the edge

Existing `icon.png` ships as a flat rounded square with a 6% corner radius;
that reads "off" next to native app icons in Finder/Dock. After this runs,
re-run `pnpm tauri icon src-tauri/icons/icon.png` to regenerate the .icns
+ .ico + per-size PNGs from the corrected source.
"""

from PIL import Image, ImageDraw, ImageFilter
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "src-tauri" / "icons" / "icon.png"
OUT = SRC  # in-place

CANVAS = 1024
# Apple's squircle radius is approximately 22.37% of the canvas (185 px on
# 1024). We round to 230 to match a slightly softer rendering that survives
# downscaling to 32×32 better than the geometrically exact value.
RADIUS = 230
# The existing artwork already has its own chrome (light bg + dark glyph
# already inset). Don't inset again — just re-mask the corners to the macOS
# template radius.
INSET = 0


def squircle_mask(size: int, radius: int) -> Image.Image:
    """Anti-aliased rounded-rect mask. Render at 4× then downsample for
    crisp edges at all icon sizes."""
    s = 4
    mask = Image.new("L", (size * s, size * s), 0)
    draw = ImageDraw.Draw(mask)
    draw.rounded_rectangle(
        (0, 0, size * s - 1, size * s - 1),
        radius=radius * s,
        fill=255,
    )
    return mask.resize((size, size), Image.LANCZOS)


def main() -> None:
    if not SRC.exists():
        print(f"::error::source not found: {SRC}", file=sys.stderr)
        sys.exit(1)

    src = Image.open(SRC).convert("RGBA")
    if src.size != (CANVAS, CANVAS):
        print(f"::error::expected {CANVAS}x{CANVAS}, got {src.size}", file=sys.stderr)
        sys.exit(1)

    if INSET > 0:
        inner = src.resize((CANVAS - 2 * INSET, CANVAS - 2 * INSET), Image.LANCZOS)
        bg_color = src.getpixel((INSET // 2, INSET // 2))
        canvas = Image.new("RGBA", (CANVAS, CANVAS), bg_color)
        canvas.paste(inner, (INSET, INSET), inner)
    else:
        canvas = src

    # Apply squircle mask. RGBA with alpha = mask gives a clean transparent
    # edge that respects whatever Finder/Dock backdrop is showing.
    mask = squircle_mask(CANVAS, RADIUS)
    out = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    out.paste(canvas, (0, 0), mask)

    out.save(OUT, format="PNG", optimize=True)
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
