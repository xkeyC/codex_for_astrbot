#!/usr/bin/env python3
"""Run the codex-rs tests for the AstrBot bridge (fork addition).

The workspace's ``default-members`` are ``codex-astrbot-py`` and
``codex-code-mode-host``, so the default scope, ``bridge``, is a plain
``cargo test``: the bridge's own unit tests, the fork's integration tests in
``astrbot-py/tests/bridge`` (chat wire, dynamic tools, files API, local-infra
voice, tool catalog, persona compaction) and the code-mode host's tests.
Upstream crates are not test-built at all.

The wider scopes are for an upstream sync. They run upstream tests of the
crates the bridge depends on (its normal dependency closure, from
``cargo metadata``):

- ``fork``: closure crates the fork changes relative to the upstream release
  it last merged (nearest ``rust-v*`` tag).
- ``changed``: closure crates touched since ``--base`` (merge base with
  ``origin/astrbot`` by default, plus uncommitted and untracked files).
- ``closure``: every closure crate.

Codex's own sandboxes and native shell execution are not compatibility
targets: their crates are left out and the upstream tests listed in
``IGNORE`` are skipped (``--no-ignore`` runs them). Tests that launch the
``codex``/``codex-exec`` executables (the CLI, not built here) are listed
separately and do not fail the run. Their output is kept in
``target/fork_test.log``.

Examples::

    python3 astrbot-py/scripts/fork_test.py            # same as `cargo test`
    python3 astrbot-py/scripts/fork_test.py --scope fork --list
    python3 astrbot-py/scripts/fork_test.py -p codex-core -- chat_wire

Arguments after ``--`` go to the test binaries (filters, ``--nocapture``).
``RUST_MIN_STACK`` and the cached V8 archive variables are filled in when
unset; ``LD_LIBRARY_PATH``/``PKG_CONFIG_PATH`` for openssl must come from
the caller's environment.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

WORKSPACE = Path(__file__).resolve().parents[2]
REPO = WORKSPACE.parent
ROOTS = ("codex-astrbot-py", "codex-code-mode-host")
ALWAYS = ("codex-astrbot-py",)

# Executables that tests of a package look up with `cargo_bin` but that live
# in another package: cargo only builds a package's own binaries for its
# tests, so these are built first. package -> [(package, bin), ...]
HELPER_BINS = {
    "codex-core": [
        ("codex-code-mode-host", "codex-code-mode-host"),
        ("codex-rmcp-client", "test_stdio_server"),
        ("codex-rmcp-client", "test_streamable_http_server"),
        ("codex-message-board-mcp", "codex-message-board-mcp"),
    ],
}

# Upstream tests this fork does not keep working. The fork's goal is the
# Python bridge; Codex's own sandboxes, native shell execution (unified exec,
# shell snapshots, apply_patch through the CLI), approval/permission grants and
# hooks are not compatibility targets (AstrBot runs commands in shipyard-neo).
# They fail on this NixOS host (no /bin/bash; the copied sandbox helper loses
# LD_LIBRARY_PATH) and on upstream rust-v0.171.0 alike.
#
# Sandbox crates are left out of every scope:
IGNORE_PACKAGES = (
    "codex-linux-sandbox",
    "codex-mxc-sandbox",
    "codex-sandboxing",
    "codex-windows-sandbox",
)
# Passed as `--skip` filters: libtest skips every test whose name contains one
# of them, in all test binaries of the run, so keep them specific.
IGNORE = {
    "codex-core": [
        # built without exec-policy rule files (execpolicy `starlark` off) and
        # without Amazon Bedrock (model-provider `aws` off)
        "exec_policy::",
        "bedrock",
        # unit tests: native shell and sandboxed exec
        "exec::tests::command_runs_after_integrity_check_failure",
        "exec::tests::kill_child_process_group_kills_grandchildren_on_timeout",
        "shell_snapshot::",
        "tasks::user_shell::tests::",
        "tools::runtimes::tests::",
        "tools::handlers::unified_exec::tests::",
        # integration suites about sandboxes, native exec and grants
        "suite::unified_exec",
        "suite::approvals::",
        "suite::shell_snapshot::",
        "suite::workspace_roots::",
        "suite::apply_patch_cli::",
        "suite::extension_sandbox::",
        "suite::network_approval::",
        "suite::permissions_messages::",
        "suite::rmcp_client::sandbox_tests::",
        "suite::rmcp_client::network_policy_compat_tests::",
        "suite::rmcp_client::stdio_mcp_tool_call_includes_sandbox_state_meta::",
        "suite::rmcp_client::local_stdio_server_uses_runtime_fallback_cwd_when_config_omits_cwd",
        "suite::agent_execution::permission_reload_tests::",
        "suite::agents_md::restricted_project_without_instructions_starts_successfully",
        "suite::cloud_config::managed_deny_read_requirements_follow_thread_permission_updates",
        "suite::guardian_review::guardian_review_session_does_not_inherit_legacy_notify",
        "suite::guardian_review::guardian_session_is_reused_for_consecutive_tool_reviews_without_prewarm",
        "suite::hooks::model_request_post_hook_orders_tool_dispatch::",
        "suite::openai_file_mcp::codex_apps_file_params_reject_denied_file_before_upload",
        "suite::openai_file_mcp::codex_apps_file_params_retry_stream_under_restricted_read_policy",
        "suite::plugins::agent_plugin_root_mcp_stdio_tool_round_trip_expands_reserved_paths",
        "suite::plugins::curated_agent_plugin_app_routing_reaches_turns::",
        "suite::plugins::plugin_metrics_stdin_works_with_sandbox_approval_disabled::",
        "suite::scenarios::astra_switches_environments_for_the_rest_of_the_active_turn",
        "suite::scenarios::catalog_permission_paths_follow_selected_model::denied_reads",
        "suite::scenarios::skill_catalog_dedup::repository_skill_turn_grant",
        "suite::skills_extension::capability_sections_render_in_order_with_host_repo_and_plugin_skills::",
        "suite::skills_extension::discovery_grants_tests::",
        "suite::skills_extension::executor_skill_tool_reads_references_under_current_permissions::",
        "suite::skills_extension::repository_skill_cache_cannot_read_inaccessible_files::",
        "suite::skills_extension::repository_skill_discovery_uses_session_grants",
        "suite::skills_extension::repository_skill_grant_excludes_a_parent_after_a_nested_repository",
        "suite::tools::exec_command_enforces_glob_deny_read_policy",
        "suite::user_notification::notify_receives_user_text_while_the_model_receives_attachments",
        "suite::view_image::view_image_tool_applies_local_sandbox_read_denies",
    ],
}

# Tests that need these executables are reported as "needs the CLI" instead
# of failing the run: building them pulls in codex-cli, the TUI and the app
# server, which AstrBot never ships. Matched against a failed test's output.
CLI_BINS = ("codex", "codex-exec")
NEEDS_CLI = re.compile(
    r'could not locate binary "(?:%s)"|NotFound \{ name: "(?:%s)"'
    % ("|".join(CLI_BINS), "|".join(CLI_BINS))
)


def _run(cmd: list[str], **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, text=True, **kwargs)


def _host_triple() -> str:
    out = _run(["rustc", "-vV"], capture_output=True).stdout
    match = re.search(r"^host: (\S+)", out, re.MULTILINE)
    if not match:
        raise RuntimeError("cannot read the host triple from `rustc -vV`")
    return match.group(1)


def _metadata() -> dict:
    out = _run(
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            _host_triple(),
        ],
        cwd=WORKSPACE,
        capture_output=True,
    ).stdout
    return json.loads(out)


def _closure(meta: dict) -> dict[str, Path]:
    """Workspace packages reachable from ROOTS over normal and build deps.

    Returns:
        Package name -> package directory.
    """
    members = set(meta["workspace_members"])
    pkgs = {p["id"]: p for p in meta["packages"] if p["id"] in members}
    by_name = {p["name"]: pid for pid, p in pkgs.items()}
    edges: dict[str, list[str]] = {}
    for node in meta["resolve"]["nodes"]:
        if node["id"] not in pkgs:
            continue
        edges[node["id"]] = [
            dep["pkg"]
            for dep in node["deps"]
            if dep["pkg"] in pkgs
            and any(kind["kind"] in (None, "build") for kind in dep["dep_kinds"])
        ]
    seen: set[str] = set()
    todo = [by_name[name] for name in ROOTS]
    while todo:
        pid = todo.pop()
        if pid in seen:
            continue
        seen.add(pid)
        todo.extend(edges.get(pid, []))
    return {
        pkgs[pid]["name"]: Path(pkgs[pid]["manifest_path"]).parent for pid in seen
    }


def _all_packages(meta: dict) -> dict[str, Path]:
    members = set(meta["workspace_members"])
    return {
        p["name"]: Path(p["manifest_path"]).parent
        for p in meta["packages"]
        if p["id"] in members
    }


def _git(*args: str) -> str:
    return _run(["git", *args], cwd=REPO, capture_output=True).stdout


def _upstream_tag() -> str:
    return _git("describe", "--tags", "--abbrev=0", "--match", "rust-v*", "HEAD").strip()


def _changed_files(base: str, worktree: bool) -> list[Path]:
    names = _git("diff", "--name-only", base, "HEAD").splitlines()
    if worktree:
        names += _git("diff", "--name-only", "HEAD").splitlines()
        names += _git("ls-files", "--others", "--exclude-standard").splitlines()
    return [REPO / name for name in names if name]


def _owners(files: list[Path], packages: dict[str, Path]) -> set[str]:
    """Map files to the workspace package whose directory holds them."""
    dirs = sorted(packages.items(), key=lambda item: len(str(item[1])), reverse=True)
    owners = set()
    for path in files:
        for name, pkg_dir in dirs:
            if path.is_relative_to(pkg_dir):
                owners.add(name)
                break
    return owners


def _fill_env(env: dict) -> None:
    env.setdefault("RUST_MIN_STACK", "16777216")
    if "RUSTY_V8_ARCHIVE" in env:
        return
    text = (WORKSPACE / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^v8\s*=\s*"=?([0-9.]+)"', text, re.MULTILINE)
    if not match:
        return
    cache = Path.home() / ".cache" / "codex-astrbot" / "v8" / match.group(1)
    archives = sorted(cache.glob("librusty_v8_*.a.gz"))
    bindings = sorted(cache.glob("src_binding_*.rs"))
    if archives and bindings:
        env["RUSTY_V8_ARCHIVE"] = str(archives[0])
        env.setdefault("RUSTY_V8_SRC_BINDING_PATH", str(bindings[0]))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument(
        "--scope", choices=("bridge", "changed", "fork", "closure"), default="bridge"
    )
    parser.add_argument(
        "--base",
        default="origin/astrbot",
        help="ref the `changed` scope diffs against (its merge base with HEAD)",
    )
    parser.add_argument(
        "-p",
        "--package",
        action="append",
        default=[],
        help="test exactly these packages (repeatable); overrides --scope",
    )
    parser.add_argument("--list", action="store_true", help="print the packages and exit")
    parser.add_argument("--no-run", action="store_true", help="build the tests, do not run them")
    parser.add_argument(
        "--no-ignore", action="store_true", help="also run the tests listed in IGNORE"
    )
    parser.add_argument("cargo_args", nargs="*", help="after `--`: test binary arguments")
    args = parser.parse_args()

    meta = _metadata()
    closure = _closure(meta)
    outside: set[str] = set()
    if not args.package and args.scope == "bridge":
        env = dict(os.environ)
        _fill_env(env)
        cmd = ["cargo", "test", "--no-fail-fast"]
        if args.list:
            print("[fork_test] bridge: workspace default-members (" + " ".join(ROOTS) + ")")
            return 0
        if args.no_run:
            cmd.append("--no-run")
        elif args.cargo_args:
            cmd += ["--", *args.cargo_args]
        print("[fork_test] " + " ".join(cmd), flush=True)
        return subprocess.run(cmd, cwd=WORKSPACE, env=env).returncode
    if args.package:
        unknown = set(args.package) - set(_all_packages(meta))
        if unknown:
            parser.error(f"not workspace packages: {', '.join(sorted(unknown))}")
        selected = set(args.package)
        label = "packages given with -p"
    elif args.scope == "closure":
        selected = set(closure)
        label = f"closure of {', '.join(ROOTS)}"
    else:
        if args.scope == "fork":
            base = _upstream_tag()
            files = _changed_files(base, worktree=True)
            label = f"fork changes since {base}"
        else:
            base = _git("merge-base", args.base, "HEAD").strip()
            files = _changed_files(base, worktree=True)
            label = f"changes since {args.base} ({base[:10]}) and uncommitted"
        touched = _owners(files, _all_packages(meta))
        selected = (touched & set(closure)) | set(ALWAYS)
        outside = touched - set(closure)

    if not args.package:
        selected -= set(IGNORE_PACKAGES)
    selected = sorted(selected)
    print(f"[fork_test] {label}: {len(selected)} of {len(closure)} closure packages")
    print("  " + " ".join(selected))
    if outside:
        print(
            "[fork_test] touched outside the AstrBot closure (not tested; on an upstream"
            " sync check them with `cargo check --tests -p ...`):\n  " + " ".join(sorted(outside))
        )
    if args.list:
        return 0

    env = dict(os.environ)
    _fill_env(env)
    helpers = sorted({h for pkg in selected for h in HELPER_BINS.get(pkg, [])})
    if helpers:
        cmd = ["cargo", "build"]
        for pkg in sorted({pkg for pkg, _ in helpers}):
            cmd += ["-p", pkg]
        for _, binary in helpers:
            cmd += ["--bin", binary]
        print("[fork_test] " + " ".join(cmd))
        _run(cmd, cwd=WORKSPACE, env=env)

    cmd = ["cargo", "test", "--no-fail-fast"]
    for pkg in selected:
        cmd += ["-p", pkg]
    if args.no_run:
        cmd.append("--no-run")
    else:
        test_args = list(args.cargo_args)
        if not args.no_ignore:
            for skip in (s for pkg in selected for s in IGNORE.get(pkg, [])):
                test_args += ["--skip", skip]
        if test_args:
            cmd += ["--", *test_args]
    print("[fork_test] " + " ".join(cmd), flush=True)
    target = Path(env.get("CARGO_TARGET_DIR") or WORKSPACE / "target")
    target.mkdir(parents=True, exist_ok=True)
    log_path = target / "fork_test.log"
    with log_path.open("w", encoding="utf-8") as log:
        proc = subprocess.Popen(
            cmd,
            cwd=WORKSPACE,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
        )
        assert proc.stdout is not None
        for line in proc.stdout:
            sys.stdout.write(line)
            log.write(line)
        code = proc.wait()
    if args.no_run:
        return code
    return _summarize(log_path.read_text(encoding="utf-8"), code, log_path)


def _summarize(output: str, code: int, log_path: Path) -> int:
    """Print totals and split failures into "needs the CLI" and real ones.

    Returns:
        0 when every failure only lacked a CLI executable, else non-zero.
    """
    totals = [0, 0, 0]
    for match in re.finditer(
        r"^test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored", output, re.M
    ):
        for i in range(3):
            totals[i] += int(match.group(i + 1))
    failed = set(re.findall(r"^test (\S+) \.\.\. FAILED$", output, re.M))
    sections = re.split(r"^---- (\S+) stdout ----$", output, flags=re.M)
    needs_cli = {
        name
        for name, body in zip(sections[1::2], sections[2::2])
        if NEEDS_CLI.search(body.split("\nfailures:\n")[0])
    }
    needs_cli &= failed
    real = sorted(failed - needs_cli)
    print(
        f"\n[fork_test] {totals[0]} passed, {totals[1]} failed, {totals[2]} ignored;"
        f" {len(needs_cli)} of the failures need the codex CLI (not built in this scope)."
    )
    if real:
        print(f"[fork_test] {len(real)} other failures:")
        for name in real:
            print("  " + name)
    # A binary killed by a signal (stack overflow, abort) reports no results
    # for the tests it had not finished.
    crashed = re.findall(r"process didn't exit successfully: `([^`]+)`.*\(signal: ", output)
    for binary in crashed:
        print(f"[fork_test] test binary crashed, its remaining tests did not run: {binary}")
    print(f"[fork_test] full output: {log_path}")
    if code != 0 and not failed:
        return code  # build error
    return 1 if real or crashed else 0


if __name__ == "__main__":
    sys.exit(main())
