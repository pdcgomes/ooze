"""Writes creatures/slime.png: badcoffee.dev's Slime at a pixel per cell, which ooze draws in the terminal.

  python3 scripts/creature.py [PRINT_FOLDER]

Reads PRINT_FOLDER/report.json and the Slime's creature.png, where every cell is a square of whole
pixels, as haunt's scripts/icons.py does for hauntop's ghost. PRINT_FOLDER is ~/src/badcoffee.dev/print
by default.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
NAME = "Slime"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("folder", nargs="?", type=Path, default=Path.home() / "src" / "badcoffee.dev" / "print")
    args = parser.parse_args()
    report = json.loads((args.folder / "report.json").read_text())
    study = next(s for s in report["studies"] if s["name"] == NAME)
    here = args.folder / Path(study["folder"]).name
    w, h = study["cells"]
    cells = Image.open(here / "creature.png").convert("RGBA").resize((w, h), Image.NEAREST)
    cells = cells.crop(cells.getbbox())
    out = ROOT / "creatures" / "slime.png"
    out.parent.mkdir(exist_ok=True)
    cells.save(out, optimize=True)
    print(f"{out.relative_to(ROOT)}: {cells.width} x {cells.height}")


if __name__ == "__main__":
    main()
