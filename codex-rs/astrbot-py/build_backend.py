"""PEP 517 backend: maturin, plus helper binaries built from this workspace.

``pip install git+https://github.com/xkeyC/codex_for_astrbot#subdirectory=codex-rs/astrbot-py``
compiles the extension module and, unless turned off, the code-mode host that
Codex re-execs, so the installed package needs no separately built binaries.
Nothing pre-compiled is shipped; everything comes from this source tree.

Environment switches (build time):

- ``CODEX_ASTRBOT_SKIP_HOST=1``  skip the code-mode host. Code mode then needs
  ``code_mode_host`` to point at a host built from this same fork.
- ``CODEX_ASTRBOT_WITH_CODEX=1`` also build the ``codex`` executable, needed
  only for native command execution and memory consolidation.
- ``CODEX_ASTRBOT_DEBUG=1``      build the helpers with the dev profile.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

from maturin import (  # noqa: F401 - re-exported as the backend's hooks
    build_sdist,
    get_requires_for_build_editable,
    get_requires_for_build_sdist,
    get_requires_for_build_wheel,
    prepare_metadata_for_build_editable,
    prepare_metadata_for_build_wheel,
)
from maturin import build_editable as _maturin_build_editable
from maturin import build_wheel as _maturin_build_wheel

HERE = Path(__file__).resolve().parent
WORKSPACE = HERE.parent
BIN_DIR = HERE / "python" / "codex_astrbot" / "bin"
# binary -> cargo package
HELPERS = {"codex-code-mode-host": "codex-code-mode-host", "codex": "codex-cli"}


def _helpers() -> list[str]:
    wanted = [] if os.environ.get("CODEX_ASTRBOT_SKIP_HOST") else ["codex-code-mode-host"]
    if os.environ.get("CODEX_ASTRBOT_WITH_CODEX"):
        wanted.append("codex")
    return wanted


def _build_helpers() -> None:
    wanted = _helpers()
    if BIN_DIR.exists():
        shutil.rmtree(BIN_DIR)
    if not wanted:
        return
    debug = bool(os.environ.get("CODEX_ASTRBOT_DEBUG"))
    env = dict(os.environ)
    if not debug:
        # The workspace release profile keeps debuginfo; drop it from the wheel.
        env.setdefault("CARGO_PROFILE_RELEASE_DEBUG", "0")
        env.setdefault("CARGO_PROFILE_RELEASE_STRIP", "symbols")
    for binary in wanted:
        command = ["cargo", "build", "-p", HELPERS[binary], "--bin", binary]
        if not debug:
            command.append("--release")
        subprocess.run(command, cwd=WORKSPACE, check=True, env=env)
    target = Path(env.get("CARGO_TARGET_DIR") or WORKSPACE / "target") / (
        "debug" if debug else "release"
    )
    exe = ".exe" if os.name == "nt" else ""
    BIN_DIR.mkdir(parents=True, exist_ok=True)
    for binary in wanted:
        shutil.copy2(target / f"{binary}{exe}", BIN_DIR / f"{binary}{exe}")


def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    _build_helpers()
    return _maturin_build_wheel(wheel_directory, config_settings, metadata_directory)


def build_editable(wheel_directory, config_settings=None, metadata_directory=None):
    _build_helpers()
    return _maturin_build_editable(wheel_directory, config_settings, metadata_directory)
