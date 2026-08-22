#!/usr/bin/env python3
"""Verify the repository toolchain before source edits or validation."""

import argparse
import json
import shutil
import subprocess
import sys
from dataclasses import asdict
from dataclasses import dataclass


EXPECTED_JUST_VERSION = "1.51.0"


@dataclass(frozen=True)
class ToolResult:
    name: str
    path: str | None
    version: str | None
    ok: bool
    error: str | None = None


TOOLS: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("just", ("--version",)),
    ("cargo", ("--version",)),
    ("rustc", ("--version",)),
    ("rustfmt", ("--version",)),
    ("cargo-clippy", ("--version",)),
    ("cargo-nextest", ("--version",)),
    ("dotslash", ("--version",)),
    ("uv", ("--version",)),
)


def check_tool(name: str, version_args: tuple[str, ...]) -> ToolResult:
    path = shutil.which(name)
    if path is None:
        return ToolResult(
            name=name, path=None, version=None, ok=False, error="not found on PATH"
        )

    try:
        completed = subprocess.run(
            [path, *version_args],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError) as error:
        return ToolResult(
            name=name,
            path=path,
            version=None,
            ok=False,
            error=f"version probe failed: {error}",
        )

    output = (completed.stdout or completed.stderr).strip()
    if completed.returncode != 0:
        return ToolResult(
            name=name,
            path=path,
            version=output or None,
            ok=False,
            error=f"version probe exited {completed.returncode}",
        )

    if name == "just" and EXPECTED_JUST_VERSION not in output.split():
        return ToolResult(
            name=name,
            path=path,
            version=output or None,
            ok=False,
            error=f"expected just {EXPECTED_JUST_VERSION}",
        )

    return ToolResult(name=name, path=path, version=output or None, ok=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Check the commands used by the repository formatting and test helpers."
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="emit machine-readable results instead of the human-readable report",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    results = [check_tool(name, version_args) for name, version_args in TOOLS]
    if args.json:
        print(
            json.dumps(
                {
                    "ok": all(result.ok for result in results),
                    "tools": [asdict(result) for result in results],
                }
            )
        )
    else:
        for result in results:
            status = "OK" if result.ok else "MISSING"
            details = f"{result.path or '-'} ({result.version or result.error})"
            print(f"{status:7} {result.name:14} {details}")

    return 0 if all(result.ok for result in results) else 1


if __name__ == "__main__":
    sys.exit(main())
