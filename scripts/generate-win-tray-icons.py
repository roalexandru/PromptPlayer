#!/usr/bin/env python3
"""Generate Windows-tray theme-adaptive icons from the canonical
`src-tauri/icons/tray-icon.png` (black-on-transparent template).

Output:
    src-tauri/icons/tray-icon-win-light.png  ← black glyph (Win light theme)
    src-tauri/icons/tray-icon-win-dark.png   ← white glyph (Win dark theme)

The light-theme variant is a straight copy of the canonical source. The
dark-theme variant inverts luminance while preserving the alpha channel so
the glyph stays anti-aliased on whatever transparent backdrop Win11's
notification flyout uses.

Run once after editing tray-icon.png; both Win-side outputs are checked in.
The runtime tray code in `src-tauri/src/app/setup.rs` picks one of them at
launch via the `SystemUsesLightTheme` registry key.
"""

from PIL import Image
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "src-tauri" / "icons" / "tray-icon.png"
OUT_LIGHT = ROOT / "src-tauri" / "icons" / "tray-icon-win-light.png"
OUT_DARK = ROOT / "src-tauri" / "icons" / "tray-icon-win-dark.png"


def main() -> None:
    if not SRC.exists():
        print(f"::error::source not found: {SRC}", file=sys.stderr)
        sys.exit(1)

    src = Image.open(SRC).convert("RGBA")

    # Light-theme variant: same as source. Copy rather than symlink so the
    # build system pulls a real file via include_bytes!.
    src.save(OUT_LIGHT, format="PNG", optimize=True)
    print(f"wrote {OUT_LIGHT.relative_to(ROOT)} ({OUT_LIGHT.stat().st_size} bytes)")

    # Dark-theme variant: invert RGB while preserving alpha. The original
    # glyph is ~black on transparent; inverted is ~white on transparent.
    # We don't blanket-invert (that would also flip transparent→opaque); we
    # iterate per-pixel and only invert RGB, leaving A untouched.
    pixels = src.load()
    w, h = src.size
    dark = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    dpx = dark.load()
    for y in range(h):
        for x in range(w):
            r, g, b, a = pixels[x, y]
            dpx[x, y] = (255 - r, 255 - g, 255 - b, a)
    dark.save(OUT_DARK, format="PNG", optimize=True)
    print(f"wrote {OUT_DARK.relative_to(ROOT)} ({OUT_DARK.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
