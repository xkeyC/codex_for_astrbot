"""In-process codex-core binding for AstrBot (codex_for_astrbot fork).

Installed wheels carry the helper executables Codex re-execs under ``bin/``
(``codex-code-mode-host`` for code mode, and ``codex`` for native execution
when built with ``CODEX_ASTRBOT_WITH_CODEX=1``); ``bundled_executable`` finds
them.
"""

from __future__ import annotations

import os
from pathlib import Path

from .codex_astrbot import Runtime

__all__ = ["Runtime", "bundled_executable"]

_BIN = Path(__file__).resolve().parent / "bin"


def bundled_executable(name: str) -> str | None:
    """Path of a helper executable shipped in this package, or None."""
    path = _BIN / (f"{name}.exe" if os.name == "nt" else name)
    return str(path) if path.is_file() else None
