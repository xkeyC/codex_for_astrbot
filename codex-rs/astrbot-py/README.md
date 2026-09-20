# codex-astrbot

In-process Python binding that lets AstrBot drive `codex-core` (fork addition).
Nothing pre-compiled is distributed: the extension module and the code-mode
host are compiled from this workspace when the package is installed.

## Install

```bash
pip install "git+https://github.com/xkeyC/codex_for_astrbot@astrbot#subdirectory=codex-rs/astrbot-py"
```

Requirements: a Rust toolchain (stable), a C/C++ linker, and network access —
the code-mode host links V8, whose prebuilt library the `v8` crate downloads
from its own upstream release during the build. Expect a long first build.

## Build switches

| Environment variable | Effect |
| --- | --- |
| `CODEX_ASTRBOT_SKIP_HOST=1` | Skip the code-mode host. Code mode then needs `code_mode_host` in the AstrBot config to point at a host built from this same fork; no V8 download. |
| `CODEX_ASTRBOT_WITH_CODEX=1` | Also build the `codex` executable, needed only for native command execution and memory consolidation. Large. |
| `CODEX_ASTRBOT_DEBUG=1` | Build the helper binaries with the dev profile. |

Helper binaries land in `codex_astrbot/bin/`; `codex_astrbot.bundled_executable(name)`
returns their installed path, which is what AstrBot looks up by default.

## Development

```bash
maturin develop            # editable install into the active virtualenv
cargo test -p codex-astrbot-py
```

`maturin develop` goes through the same backend, so it builds the helper
binaries too; the switches above apply.
