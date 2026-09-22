# codex-astrbot

In-process Python binding that lets AstrBot drive `codex-core` (fork addition).
Nothing pre-compiled is distributed: the extension module and the code-mode
host are compiled from this workspace when the package is installed.

## Install

```bash
pip install "git+https://github.com/xkeyC/codex_for_astrbot@astrbot#subdirectory=codex-rs/astrbot-py"
```

Requirements: a Rust toolchain (stable), a C/C++ linker, and network access.
Expect a long first build.

The code-mode host links V8. The `v8` crate fetches its prebuilt archive from
denoland/rusty_v8, which does not publish the version Codex pins, so the build
would fail with a 404; the build backend fetches the archive Codex publishes
instead (about 30 MB), checks it against the published checksum and caches it
under `~/.cache/codex-astrbot/v8/<version>/`.

## Build switches

| Environment variable | Effect |
| --- | --- |
| `CODEX_ASTRBOT_SKIP_HOST=1` | Skip the code-mode host. Code mode then needs `code_mode_host` in the AstrBot config to point at a host built from this same fork; no V8 download. |
| `CODEX_ASTRBOT_WITH_CODEX=1` | Also build the `codex` executable, needed only for native command execution and memory consolidation. Large. |
| `CODEX_ASTRBOT_DEBUG=1` | Build the helper binaries with the dev profile. |
| `RUSTY_V8_ARCHIVE=<path>` | Use a V8 archive already on disk instead of downloading one. |
| `CODEX_ASTRBOT_V8_BASE_URL=<url>` | Fetch the V8 archive from a mirror. |
| `CODEX_ASTRBOT_V8_CACHE=<dir>` | Where downloaded V8 archives are kept. |

Helper binaries land in `codex_astrbot/bin/`; `codex_astrbot.bundled_executable(name)`
returns their installed path, which is what AstrBot looks up by default.

## Realtime voice

`Runtime.realtime_start(thread_id, request_json)` attaches a realtime (voice)
conversation to a loaded thread; the realtime model hands tasks off to that
thread. With `{"transport": {"type": "webrtc", "sdp": offer}}` the host keeps
the media (e.g. with aiortc) and receives the answer as a
`realtime_conversation_sdp` event; this transport works with a ChatGPT account,
while `websocket` needs an API key. `realtime_append_text`,
`realtime_append_speech`, `realtime_append_audio` (websocket only),
`realtime_stop` and `Runtime.realtime_list_voices()` cover the rest; realtime
events come through `next_event` like any other.

`Runtime.create` accepts `"originator"` to send another client identity
(`originator` header and User-Agent), e.g. `codex-tui` as the official TUI does.

## Development

```bash
maturin develop            # editable install into the active virtualenv
cargo test -p codex-astrbot-py
```

`maturin develop` goes through the same backend, so it builds the helper
binaries too; the switches above apply.
