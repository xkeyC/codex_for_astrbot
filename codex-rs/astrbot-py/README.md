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
