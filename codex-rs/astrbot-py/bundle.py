"""Build a codex_astrbot wheel that bundles its helper executables.

    python bundle.py [--debug] [--develop]

Builds ``codex-code-mode-host`` and ``codex`` from this workspace, copies
them into ``python/codex_astrbot/bin/`` and runs ``maturin build`` (or
``maturin develop``). Code mode needs rusty_v8: set RUSTY_V8_ARCHIVE and
RUSTY_V8_SRC_BINDING_PATH as for any codex-code-mode build.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
WORKSPACE = HERE.parent
HELPERS = {"codex-code-mode-host": "codex-code-mode-host", "codex-cli": "codex"}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--debug", action="store_true", help="debug profile")
    ap.add_argument("--develop", action="store_true", help="maturin develop")
    args = ap.parse_args()
    profile = [] if args.debug else ["--release"]
    if not args.debug:
        # The workspace release profile keeps debuginfo; drop it from the wheel.
        os.environ.setdefault("CARGO_PROFILE_RELEASE_DEBUG", "0")
        os.environ.setdefault("CARGO_PROFILE_RELEASE_STRIP", "symbols")
    exe = ".exe" if os.name == "nt" else ""
    target = WORKSPACE / "target" / ("debug" if args.debug else "release")

    for package, binary in HELPERS.items():
        subprocess.run(
            ["cargo", "build", "-p", package, "--bin", binary, *profile],
            cwd=WORKSPACE,
            check=True,
        )
    bin_dir = HERE / "python" / "codex_astrbot" / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    for binary in HELPERS.values():
        shutil.copy2(target / f"{binary}{exe}", bin_dir / f"{binary}{exe}")

    maturin = ["maturin", "develop" if args.develop else "build", *profile]
    return subprocess.run(maturin, cwd=HERE).returncode


if __name__ == "__main__":
    sys.exit(main())
