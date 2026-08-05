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
import tempfile

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


class Unreadable(Exception):
    """A tracked file the check could not read. Never silently treated as clean."""

    def __init__(self, path: str, reason: str) -> None:
        super().__init__(f"{path}: {reason}")
        self.path = path
        self.reason = reason


def offenders(path: str) -> list[tuple[int, int, int]]:
    """Scan one tracked file, or raise [`Unreadable`].

    A read failure must never return `[]`: that is indistinguishable from "clean"
    and would make the gate pass by accident on exactly the file it could not
    inspect. A check that reports success when it did no work is worse than no
    check, because it is trusted. The one benign case is a tracked file deleted
    from the working tree (`git rm` staged, or a mid-rebase state) -- there is no
    content to scan, so the caller skips it explicitly.
    """
    try:
        with open(path, "rb") as handle:
            return scan(handle.read())
    except FileNotFoundError as exc:
        raise Unreadable(path, "tracked but missing from the working tree") from exc
    except OSError as exc:
        raise Unreadable(path, exc.strerror or exc.__class__.__name__) from exc


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

    # `offenders` must raise rather than return [] on an unreadable path: a silent []
    # would make the gate report "clean" for a file it never inspected.
    total += 1
    missing = os.path.join(tempfile.gettempdir(), "check-control-chars-does-not-exist")
    try:
        got_hits = offenders(missing)
    except Unreadable as exc:
        if not isinstance(exc.__cause__, FileNotFoundError):
            failed += 1
            print(
                f"  FAIL unreadable path: want FileNotFoundError cause, got {exc.__cause__!r}",
                file=sys.stderr,
            )
    except OSError as exc:
        failed += 1
        print(f"  FAIL unreadable path: leaked a raw OSError: {exc!r}", file=sys.stderr)
    else:
        failed += 1
        print(
            f"  FAIL unreadable path: returned {got_hits!r} instead of raising -- a "
            "missing file must never look clean",
            file=sys.stderr,
        )

    # A directory is the other real read failure, and must not be reported as clean.
    total += 1
    with tempfile.TemporaryDirectory() as tmp:
        try:
            got_hits = offenders(tmp)
        except Unreadable:
            pass
        except OSError as exc:
            failed += 1
            print(f"  FAIL unreadable dir: leaked a raw OSError: {exc!r}", file=sys.stderr)
        else:
            failed += 1
            print(
                f"  FAIL unreadable dir: returned {got_hits!r} instead of raising",
                file=sys.stderr,
            )

    if failed:
        print(f"self-test: {failed} of {total} cases failed", file=sys.stderr)
        return 1
    print(f"self-test: {total} cases passed")
    return 0


def main() -> int:
    if "--self-test" in sys.argv[1:]:
        return self_test()

    files = tracked_source_files()
    bad: dict[str, list[tuple[int, int, int]]] = {}
    missing: list[str] = []
    unreadable: list[Unreadable] = []
    for path in files:
        try:
            if hits := offenders(path):
                bad[path] = hits
        except Unreadable as exc:
            # A staged deletion leaves no content to scan, which is benign; anything
            # else means the gate could not do its job and must not report success.
            if isinstance(exc.__cause__, FileNotFoundError):
                missing.append(exc.path)
            else:
                unreadable.append(exc)

    if unreadable:
        print("check-control-chars: could not read tracked files\n", file=sys.stderr)
        for exc in unreadable:
            print(f"  {exc.path}: {exc.reason}", file=sys.stderr)
        print(
            "\nThe check refuses to report success on files it could not inspect.",
            file=sys.stderr,
        )
        return 1

    if not bad:
        skipped = f", {len(missing)} skipped (not in the working tree)" if missing else ""
        print(f"check-control-chars: {len(files) - len(missing)} source files clean{skipped}")
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
