# codex-astrbot

In-process Python binding (pyo3) that lets AstrBot drive `codex-core`: package
`codex-astrbot`, module `codex_astrbot`. It is a fork addition of
[xkeyC/codex_for_astrbot](https://github.com/xkeyC/codex_for_astrbot).
Nothing pre-compiled is distributed: the extension module and the code-mode
host are compiled from this workspace when the package is installed.

## Install

From git (AstrBot pins a commit the same way):

```bash
pip install "git+https://github.com/xkeyC/codex_for_astrbot@astrbot#subdirectory=codex-rs/astrbot-py"
```

From a checkout, editable, into the active virtualenv:

```bash
uv pip install -e codex-rs/astrbot-py    # or: pip install -e codex-rs/astrbot-py
```

Requirements: the Rust toolchain pinned in `codex-rs/rust-toolchain.toml`
(rustup picks it up), a C/C++ linker, Python 3.10+ and network access. Expect
a long first build. Compiled crates are kept in `~/.cache/codex-astrbot/target/`
(several GB), so later installs reuse the compiled dependencies and rebuild
only Codex's own crates; delete the directory to reclaim the space.

The code-mode host links V8. The `v8` crate fetches its prebuilt archive from
denoland/rusty_v8, which does not publish the version Codex pins, so the
build would fail with a 404; the build backend instead fetches the archive and
its generated binding from Codex's own `rusty-v8-v<version>` release (about
30 MB), checks them against the published checksum and caches them under
`~/.cache/codex-astrbot/v8/<version>/`. Offline, download both yourself and
set `RUSTY_V8_ARCHIVE` and `RUSTY_V8_SRC_BINDING_PATH`.

## Build switches

| Environment variable | Effect |
| --- | --- |
| `CODEX_ASTRBOT_SKIP_HOST=1` | Skip the code-mode host binary. Code mode then needs AstrBot's `code_mode_host` to point at a host built from this same fork. The V8 archive is still fetched: the extension's build compiles the host's crates. |
| `CODEX_ASTRBOT_DEBUG=1` | Build the helper binaries with the dev profile. |
| `CARGO_PROFILE_RELEASE_OPT_LEVEL=3` | Build the wheel optimised for speed instead of size (default `s`: about a third smaller and faster to compile). |
| `RUSTY_V8_ARCHIVE=<path>`, `RUSTY_V8_SRC_BINDING_PATH=<path>` | Use a V8 archive and binding already on disk. |
| `CODEX_ASTRBOT_V8_BASE_URL=<url>` | Fetch the V8 files from a mirror. |
| `CODEX_ASTRBOT_V8_CACHE=<dir>` | Where downloaded V8 files are kept. |
| `CODEX_ASTRBOT_TARGET_DIR=<dir>` | Where compiled crates are kept between installs (`CARGO_TARGET_DIR` wins when set). |

Helper binaries land in `codex_astrbot/bin/`; `codex_astrbot.bundled_executable(name)`
returns their installed path, which is where AstrBot looks for
`codex-code-mode-host` first.

## How AstrBot uses it

`Runtime.create(options_json)` takes `codex_home`, `config` (dotted overrides
applied to every thread, like `-c key=value`), `code_mode_host` and
`originator`. Threads are
started or resumed with `cwd`, `model`, `model_provider`, `base_instructions`,
`developer_instructions`, per-thread `config`, `dynamic_tools`, `ephemeral`,
`no_environment` and (resume) `rollout_path`. Turns go through `submit_turn`
(with `additional_context`, `dynamic_tools`, `model`, `effort` and permission
`scopes`), events through `next_event`; `dynamic_tool_response`,
`set_dynamic_tools`, `review_decision`, `consolidate_memories`, account login
and `list_models` cover the rest.

Codex's own execution is off in AstrBot: threads start with
`no_environment` and `features.shell_tool = false`, and AstrBot supplies
`exec_command`, `write_stdin` and `apply_patch` as dynamic tools running in
its own sandbox. The binding has no exec environment at all (no `codex`
executable, no command approvals); memory consolidation then edits the memory files
through file tools of the memories extension instead of a shell. Code mode
(`code_mode_only` by default) does need `codex-code-mode-host`.

Requests carry the `codex exec` identity (`originator: codex_exec`) and a
User-Agent such as
`codex_exec/0.166.0 (Linux 22.04; x86_64) unknown; xkeyC/codex_for_astrbot`.
`originator` sends another client identity (header and User-Agent prefix), e.g.
`codex-tui`; the User-Agent keeps the fork tag, and only the first runtime of a
process can set it. Resumed threads keep the originator recorded in their
rollout, except the binding's old `astrbot`, which gives way to the current one.

## Realtime voice

`Runtime.realtime_start(thread_id, request_json)` attaches a realtime (voice)
conversation to a loaded thread. With the default OpenAI backend the realtime
model hands tasks off to the thread. `{"transport": {"type": "webrtc", "sdp":
offer}, "version": "v3"}` keeps the media in the host (e.g. aiortc); the answer
arrives as a `realtime_conversation_sdp` event. WebRTC works with a ChatGPT
account (which only accepts v3), `websocket` needs an API key. A failed start
arrives as a `realtime_conversation_realtime` event with an `Error` payload.
`realtime_append_text` (roles `user`, `developer`, `assistant`, `context`,
`voice_session`, `voice_event`), `realtime_append_speech`,
`realtime_append_audio` (websocket only), `realtime_stop` and
`Runtime.realtime_list_voices()` cover the rest.

With `[realtime] backend = "local_multimodal_infra"` the audio goes to a
local-multimodal-infra server's `/v1/realtime` WebSocket (VAD, ASR, TTS,
barge-in) and the thread answers with ordinary turns on its own provider and
model.

## Fork switches

Every codex-rs change of the fork is additive and off by default, so upstream
behavior is unchanged unless a key below is set. AstrBot sets them through
`config`.

| Key | Effect |
| --- | --- |
| `realtime.backend = "local_multimodal_infra"` | Realtime voice over local-multimodal-infra instead of an OpenAI realtime model. |
| `[realtime.local_infra]` | That server: `url`, `token`, `session` (extra `session.start` fields), `ref_audio_path` / `ref_text` (voice clone), `silence_tool`, `utc_offset_minutes`, `idle_compact_percent`. |
| `realtime.host_routes_handoffs` | The host runs realtime handoffs itself; Codex only reports them. |
| `model_provider_options.<id>.wire = "chat"` | Talk `/chat/completions` to a provider without a working Responses API; with `extra_body`, `extra_body_remove`. |
| `model_provider_options.<id>.compaction` | `auto` (default), `local` or `remote` compaction for that provider. |
| `model_provider_options.<id>.models.<slug>` | Model metadata (context window, reasoning levels, modalities, ...) for models Codex does not know. |
| `model_provider_options.<id>.files_api`, `.omit_turn_metadata` | Send images once through the provider's Files API; leave out the turn-metadata header. |
| `model_provider_options.<id>.open_model` | The provider serves an open-weight model (Responses or chat wire): code mode's `exec` is offered as a `run_code({code, description})` function tool with a TypeScript SDK description, `inspect(...)` and a nested `tool_search` for deferred tools. |
| `model_provider_options.<id>.open_model_switch_notice` | Text of the one-time notice below, in place of the default (any provider, `open_model` or not). |
| `model_tool_mode` | Force `direct`, `code_mode` or `code_mode_only` regardless of the model catalog. |
| `features.code_mode.compact_exec_description` | Shorter generic part of the `exec` description. |
| `features.code_mode.exec_as_function_tool` | `exec` as a plain function tool for providers without custom (grammar) tools. |
| `features.code_mode.structured_dynamic_tool_results` | Dynamic tool results reach scripts as `{content, isError, text}`. |
| `features.code_mode.tool_catalog` | List deferred tools in history, appending loads and unloads. |
| `features.code_mode.host_namespace` | The host's tool namespace root (`"astrbot"`). In code mode its tools drop the root: `astrbot` + `exec_command` is `tools.exec_command`; a plugin's keep the plugin layer (`astrbot__live_check` + `lookup_order_status` is `tools.live_check__lookup_order_status`), and a plugin's namesake tool merges into one name (`astrbot__qq_file` + `qq_file` is `tools.qq_file`) where no other tool has it. A host name another tool also has, or `exec`/`wait`/`run_code`, is left to neither: the host tool is `astrbot_<name>`, the other `codex_<name>`. These names are used everywhere the model sees tools (exec / run_code declarations, `ALL_TOOLS`, the tool catalog, which then lists whole names, tool search); calls still go to the real tools. Unset: names as before. |
| `additional_context.reinject_after_compaction`, `.max_value_tokens` | Put turn additional context back after compaction; per-value token budget. |
| `memories.scope_key`, `.may_write_global`, `.may_delete`, `.turn_scopes`, `.auto_consolidate` | Per-chat memory scopes next to the global store, write and delete permissions (optionally from turn `scopes`), automatic consolidation switch. |
| `memories.extra_session_sources` | Custom session sources (AstrBot's `astrbot`) whose threads feed memory extraction. |
| `outbound_proxy` | Proxy for all of Codex's HTTP and WebSocket traffic. |

Always on, for every provider: encrypted history belongs to the provider and account that produced it
(provider id plus ChatGPT account id, or a short hash of the API key or
bearer token; the secret is never stored). Each item with encrypted content
records its owner in its history metadata (kept in the rollout and through
compaction). Requests send the current identity's items back unchanged and
leave out other identities' (encrypted reasoning and compaction dropped,
encrypted parts of agent messages and tool outputs replaced by a
placeholder), and the first turn after such a switch gets a user-role
notice, once per switch. Threads from before this have no owners: their
items count as their `SessionMeta` provider's with an unknown account, so
only a different provider id makes them foreign.

Protocol additions: a dynamic tool response may set `end_turn` (and `speak`)
to end the turn without another model request; `set_dynamic_tools` /
`submit_turn` replace a thread's dynamic tools with history kept.

## Development

```bash
uv pip install -e codex-rs/astrbot-py           # repo root; rebuilds the module and the helpers
cd codex-rs && cargo test                       # the bridge's tests
```

The Python bridge is the workspace default. `default-members` is
`astrbot-py` + `code-mode-host`, and `codex-astrbot-py` enables its `python`
feature by default (`--no-default-features` for the plain Rust engine). So a
bare `cargo build`/`cargo test` in `codex-rs` builds the bridge and the host it
re-execs, and tests only them: the engine's unit tests, the fork's
integration tests in `astrbot-py/tests/bridge` (chat wire, dynamic tools,
files API, local-infra voice, tool catalog, persona compaction) and the
host's tests. Upstream crates are compiled as dependencies but their tests are
not built. The other crates (TUI, app server, CLI, exec, ...) are kept for
reference; name them with `-p`.

The crate is an rlib; maturin adds `--crate-type cdylib` when it builds the
wheel, so cargo never links the large debug extension module.

To slim the bridge, heavy upstream parts it does not use are behind features
that are off by default:

| Crate | Feature | Without it |
| --- | --- | --- |
| `codex-execpolicy` | `starlark` | no Starlark interpreter; non-empty exec-policy rule files are rejected |
| `codex-model-provider` | `aws` | no Amazon Bedrock providers (AWS SDK) |
| `codex-code-mode-host` | `ui-plugin-tests` | the UI-plugin isolate tests are not built |

`app-server` (reference) turns `aws` back on.

The wheel's two cargo builds (host binary, extension module) resolve the same
dependency features (`codex-astrbot-py` depends on the host's lib, and the host
is built with `-p codex-astrbot-py --features extension-module`), so shared
crates compile once; bundled sqlite and zstd build at `-O1` in release.

For an upstream sync, `astrbot-py/scripts/fork_test.py` also runs upstream
tests of the crates the bridge depends on:

| Command | Tests |
| --- | --- |
| `fork_test.py` | same as `cargo test` |
| `fork_test.py --scope fork` | bridge dependencies the fork changes relative to the `rust-v*` tag it last merged |
| `fork_test.py --scope changed` | bridge dependencies changed since the merge base with `origin/astrbot`, uncommitted included |
| `fork_test.py --scope closure` | every bridge dependency |
| `fork_test.py -p codex-core -- chat_wire` | named packages; arguments after `--` go to the test binaries |

Codex's own sandboxes, native shell execution, approval grants, hooks, exec
policy rules and Bedrock are not compatibility targets: the upstream tests
about them listed in the script's `IGNORE` are skipped (`--no-ignore` runs
them). Tests that launch the `codex` CLI are reported as "need the codex CLI"
and do not fail the run. `--list` prints the packages; fork-touched crates
outside the closure are listed so a sync can `cargo check --tests -p` them.
The output goes to `target/fork_test.log`.

`maturin develop` builds only the extension module, not the helper binaries;
copy a rebuilt `codex-code-mode-host` into `python/codex_astrbot/bin/` yourself
if you use it. `--workspace` builds and tests need
`--exclude codex-voice-host` unless GStreamer is installed, and the V8
variables above set to the cached files. On Windows run codex-core tests with
`RUST_MIN_STACK=16777216`.

## Syncing with upstream

The `astrbot` branch takes openai/codex releases by merging the `rust-vX.Y.Z`
tag (on a `sync/rust-vX.Y.Z` branch, then fast-forward `astrbot`), never by
rebasing. Release tags sit on release branches, so for files the fork never
touched (`git diff rust-v<old> astrbot --name-only`) take the new tag's
version, including files that merged without conflict. The repository's
top-level `README.md` stays upstream's.
