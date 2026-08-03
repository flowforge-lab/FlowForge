#!/usr/bin/env python3
"""Reject disallowed control characters in tracked source files.

A literal NUL (or other stray control byte) in a source file is not a style
nit: git classifies the file as binary, and from then on `git diff` prints
"Binary files ... differ", the GitHub review UI shows nothing, `git grep` skips
the file, and inline review comments cannot be anchored to it. A reviewer
loses the ability to see the change at all.

This shipped once (#1169, caught in review on #1185): two files used a real
0x00 byte as a cache-key separator where the escape `\\0` was meant. The
runtime behaviour is identical -- `` `${a}\\0${b}` `` and a literal NUL compare
equal -- so nothing failed. Prettier does not care, tsc does not care, and the
tests passed. Only the review process broke.

Note `git grep -I` is useless here: -I means "skip binary files", which is
exactly the set we are hunting for. This scans bytes directly instead.
"""

from __future__ import annotations

import os
import subprocess
import sys

# Text formats where a control byte is always a mistake. Deliberately an
# allowlist: binary fixtures, icons and lockfiles are none of our business.
SOURCE_SUFFIXES = (
    ".rs",
    ".ts",
    ".tsx",
    ".js",
    ".jsx",
    ".mjs",
    ".cjs",
    ".css",
    ".json",
    ".toml",
    ".yml",
    ".yaml",
    ".md",
    ".sh",
    ".py",
    ".html",
)

# Tab (0x09), LF (0x0a) and CR (0x0d) are legitimate. Everything else below
# 0x20 is not: NUL breaks git's text detection, and the rest (vertical tab,
# form feed, ESC, ...) are invisible in an editor and carry no meaning in
# source. DEL (0x7f) is excluded for the same reason.
ALLOWED = {0x09, 0x0A, 0x0D}
DISALLOWED = {b for b in range(0x20) if b not in ALLOWED} | {0x7F}

NAMES = {0x00: "NUL", 0x0B: "VT", 0x0C: "FF", 0x1B: "ESC", 0x7F: "DEL"}


def tracked_source_files() -> list[str]:
    # `git ls-files` is relative to the cwd, so running this from apps/desktop
    # would silently skip crates/ and miss Rust entirely. Anchor to the repo
    # root so the check covers the same set no matter where it is invoked.
    root = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        check=True,
        text=True,
    ).stdout.strip()
    os.chdir(root)

    out = subprocess.run(
        ["git", "ls-files", "-z"],
        capture_output=True,
        check=True,
    ).stdout
    return [
        path
        for path in (raw.decode("utf-8", "surrogateescape") for raw in out.split(b"\0"))
        if path.endswith(SOURCE_SUFFIXES)
    ]


def scan(data: bytes) -> list[tuple[int, int, int]]:
    """Return (line, column, byte) for each disallowed byte in `data`.

    Pure, so the line/column arithmetic is testable without touching a file.
    """
    if not DISALLOWED.intersection(data):
        return []

    found = []
    line = 1
    col = 1
    for byte in data:
        if byte in DISALLOWED:
            found.append((line, col, byte))
        if byte == 0x0A:
            line += 1
            col = 1
        else:
            col += 1
    return found


def offenders(path: str) -> list[tuple[int, int, int]]:
    try:
        with open(path, "rb") as handle:
            return scan(handle.read())
    except OSError:
        return []


def self_test() -> int:
    """Cases derived from the real #1185 regression and its near misses."""
    cases: list[tuple[str, bytes, list[tuple[int, int, int]]]] = [
        ("plain source", b"const a = 1;\n", []),
        ("tab indent", b"fn main() {\n\tlet a = 1;\n}\n", []),
        ("crlf line endings", b"const a = 1;\r\nconst b = 2;\r\n", []),
        # The bytes that made #1185's files "binary" to git.
        ("nul on line 1", b"`${lang}\x00${code}`\n", [(1, 9, 0x00)]),
        ("nul on a later line", b"a\nb\n`x\x00y`\n", [(3, 3, 0x00)]),
        ("escape sequence is fine", b'const s = "\\0";\n', []),
        ("vertical tab", b"a\x0bb\n", [(1, 2, 0x0B)]),
        ("escape char", b"a\x1bb\n", [(1, 2, 0x1B)]),
        ("del", b"a\x7fb\n", [(1, 2, 0x7F)]),
        ("several on one line", b"a\x00b\x00c\n", [(1, 2, 0x00), (1, 4, 0x00)]),
        # A NUL is one byte but em-dash is three and an emoji four; column
        # numbers are byte offsets, and multi-byte text must not trip the scan.
        ("multi-byte utf-8", "// \u2014 \U0001f680\n".encode(), []),
        ("nul after multi-byte", "\u2014\x00\n".encode(), [(1, 4, 0x00)]),
    ]

    failed = 0
    for name, data, want in cases:
        got = scan(data)
        if got != want:
            failed += 1
            print(f"  FAIL {name}: want {want}, got {got}", file=sys.stderr)

    total = len(cases)
    if failed:
        print(f"self-test: {failed} of {total} cases failed", file=sys.stderr)
        return 1
    print(f"self-test: {total} cases passed")
    return 0


def main() -> int:
    if "--self-test" in sys.argv[1:]:
        return self_test()

    files = tracked_source_files()
    bad = {path: hits for path in files if (hits := offenders(path))}

    if not bad:
        print(f"check-control-chars: {len(files)} source files clean")
        return 0

    print("check-control-chars: disallowed control characters found\n", file=sys.stderr)
    for path, hits in sorted(bad.items()):
        for line, col, byte in hits:
            name = NAMES.get(byte, f"0x{byte:02x}")
            print(f"  {path}:{line}:{col}: {name} (0x{byte:02x})", file=sys.stderr)

    print(
        "\nA control byte makes git treat the file as binary, which hides the diff\n"
        "from review, breaks `git grep`, and blocks inline comments. If you meant a\n"
        "NUL separator in a string, write the escape (\\0) instead of the raw byte.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
