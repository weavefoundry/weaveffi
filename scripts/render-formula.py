#!/usr/bin/env python3
"""Render packaging/homebrew/weaveffi.rb.in for a release.

usage: render-formula.py VERSION DIST_DIR [--base-url URL] [--output FILE]

DIST_DIR holds the release archives weaveffi-<target>.tar.gz for the macOS
and Linux targets below. Their SHA-256 checksums go into the formula with
the URLs they're published at (by default, the assets of the GitHub release
tagged v<VERSION>).
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TEMPLATE = ROOT / "packaging" / "homebrew" / "weaveffi.rb.in"
TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
)


def render(version: str, dist: Path, base_url: str | None = None) -> str:
    base = (base_url or f"https://github.com/weavefoundry/weaveffi/releases/download/v{version}").rstrip("/")
    text = TEMPLATE.read_text()
    for target in TARGETS:
        name = f"weaveffi-{target}.tar.gz"
        archive = dist / name
        if not archive.is_file():
            raise SystemExit(f"render-formula: {archive} is missing")
        key = target.upper().replace("-", "_")
        text = text.replace(f"@URL_{key}@", f"{base}/{name}")
        text = text.replace(f"@SHA256_{key}@", hashlib.sha256(archive.read_bytes()).hexdigest())
    left = re.findall(r"@[A-Z0-9_]+@", text)
    if left:
        raise SystemExit(f"render-formula: placeholders left in the formula: {', '.join(sorted(set(left)))}")
    return text


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("version", help="the release version, with or without a leading v")
    parser.add_argument("dist", type=Path, help="the directory that holds the release archives")
    parser.add_argument("--base-url", help="the URL the archives are published under")
    parser.add_argument("--output", type=Path, help="the file to write (standard output by default)")
    args = parser.parse_args(argv)
    formula = render(args.version.removeprefix("v"), args.dist, args.base_url)
    if args.output:
        args.output.write_text(formula)
    else:
        sys.stdout.write(formula)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
