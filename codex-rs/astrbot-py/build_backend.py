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

import hashlib
import os
import re
import shutil
import subprocess
import urllib.error
import urllib.request
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


def _v8_version() -> str:
    """Read the v8 crate version this workspace pins.

    Returns:
        The version string, for example "150.4.0".

    Raises:
        RuntimeError: If the workspace does not pin v8.
    """
    text = (WORKSPACE / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^v8\s*=\s*"=?([0-9.]+)"', text, re.MULTILINE)
    if not match:
        raise RuntimeError("Cannot find the v8 version in the workspace Cargo.toml")
    return match.group(1)


def _ensure_v8_archive(env: dict) -> None:
    """Point the v8 crate at the prebuilt archive Codex publishes.

    The code-mode host links V8. The `v8` crate downloads its prebuilt archive
    from denoland/rusty_v8, which does not publish the version Codex pins, so
    an ordinary build fails with a 404 and a misleading suggestion to compile
    V8 from source. Codex publishes the archives itself; this fetches the one
    for the host target, verifies it against the published checksum and caches
    it.

    Set RUSTY_V8_ARCHIVE to use a file that is already on disk, or
    CODEX_ASTRBOT_V8_BASE_URL to fetch from a mirror.

    Args:
        env: Environment passed to cargo; RUSTY_V8_ARCHIVE is set on it.

    Raises:
        RuntimeError: If the target cannot be resolved, or the archive cannot
            be downloaded or fails its checksum.
    """
    if env.get("RUSTY_V8_ARCHIVE"):
        return
    probe = subprocess.run(
        ["rustc", "-vV"], capture_output=True, text=True, check=True
    )
    target = next(
        (
            line.split("host: ", 1)[1].strip()
            for line in probe.stdout.splitlines()
            if line.startswith("host: ")
        ),
        "",
    )
    if not target:
        raise RuntimeError("Cannot determine the host target from `rustc -vV`")

    version = _v8_version()
    stem = f"rusty_v8_ptrcomp_sandbox_release_{target}"
    archive = f"{stem}.lib.gz" if target.endswith("windows-msvc") else f"lib{stem}.a.gz"
    cache = Path(
        env.get("CODEX_ASTRBOT_V8_CACHE")
        or Path.home() / ".cache" / "codex-astrbot" / "v8" / version
    )
    path = cache / archive
    base = env.get(
        "CODEX_ASTRBOT_V8_BASE_URL",
        f"https://github.com/openai/codex/releases/download/rusty-v8-v{version}",
    )

    if not path.is_file():
        cache.mkdir(parents=True, exist_ok=True)
        print(f"Downloading {base}/{archive}", flush=True)
        try:
            with urllib.request.urlopen(f"{base}/{archive}") as response:
                payload = response.read()
            with urllib.request.urlopen(f"{base}/{stem}.sha256") as response:
                checksums = response.read().decode("utf-8")
        except (urllib.error.URLError, OSError) as err:
            raise RuntimeError(
                f"Failed to download the prebuilt V8 archive for {target} from "
                f"{base}. Download {archive} yourself and set RUSTY_V8_ARCHIVE to "
                "it, or set CODEX_ASTRBOT_SKIP_HOST=1 to build without code "
                f"mode's host: {err}"
            ) from err
        expected = next(
            (
                line.split()[0]
                for line in checksums.splitlines()
                if line.strip().endswith(archive)
            ),
            "",
        )
        actual = hashlib.sha256(payload).hexdigest()
        if expected and actual != expected:
            raise RuntimeError(
                f"Checksum mismatch for {archive}: expected {expected}, got {actual}"
            )
        path.write_bytes(payload)
    env["RUSTY_V8_ARCHIVE"] = str(path)


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
    if "codex-code-mode-host" in wanted:
        _ensure_v8_archive(env)
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
