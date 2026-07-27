#!/usr/bin/env python3
"""Rewrite the Homebrew formula for a published release.

Usage: update_formula.py <version> <sha256sums-file>

The checksum file is `sha256sum` output over the release archives, so each line
is "<digest>  ./sesh-<version>-<target>.tar.gz". Every target the formula
declares must be present, otherwise the formula would point at an archive that
does not exist and `brew install` would fail after the release is published.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
FORMULA = ROOT / "Formula" / "sesh.rb"

TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
)

DOWNLOAD = "https://github.com/thomasindrias/sesh/releases/download"


def read_digests(path: pathlib.Path, version: str) -> dict[str, str]:
    digests: dict[str, str] = {}
    for line in path.read_text().splitlines():
        parts = line.split()
        if len(parts) != 2:
            continue
        digest, name = parts[0], pathlib.PurePath(parts[1]).name
        for target in TARGETS:
            if name == f"sesh-{version}-{target}.tar.gz":
                digests[target] = digest
    missing = [target for target in TARGETS if target not in digests]
    if missing:
        raise SystemExit(f"no checksum for: {', '.join(missing)}")
    return digests


def rewrite(text: str, version: str, digests: dict[str, str]) -> str:
    text, count = re.subn(
        r'^(\s*version\s+")[^"]+(")',
        rf"\g<1>{version}\g<2>",
        text,
        count=1,
        flags=re.M,
    )
    if count != 1:
        raise SystemExit("formula has no version line")

    # Each url line ends in its own target, so both the URL and the sha256 on
    # the following line are rebuilt from that target rather than by position.
    for target, digest in digests.items():
        archive = f"sesh-{version}-{target}.tar.gz"
        pattern = (
            rf'(url\s+")[^"]*-{re.escape(target)}\.tar\.gz("\s*\n\s*sha256\s+")[0-9a-f]{{64}}(")'
        )
        replacement = rf"\g<1>{DOWNLOAD}/v{version}/{archive}\g<2>{digest}\g<3>"
        text, count = re.subn(pattern, replacement, text)
        if count != 1:
            raise SystemExit(f"expected exactly one url/sha256 pair for {target}, found {count}")
    return text


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    version, sums = sys.argv[1], pathlib.Path(sys.argv[2])
    text = rewrite(FORMULA.read_text(), version, read_digests(sums, version))

    stale = sorted(set(re.findall(r"/download/(v[^/]+)/", text)) - {f"v{version}"})
    if stale:
        raise SystemExit(f"formula still points at other tags: {stale}")
    if "0" * 64 in text:
        raise SystemExit("formula still contains a placeholder checksum")

    FORMULA.write_text(text)
    print(f"formula updated to {version}")


if __name__ == "__main__":
    main()
