use super::*;
use pretty_assertions::assert_eq;

fn lookup(names: &CodeModeNames, tool: &ToolName) -> String {
    names
        .get(&tool.clone().with_default_namespace())
        .cloned()
        .expect("named")
}

#[test]
fn the_root_is_dropped_and_the_plugin_layer_kept() {
    let exec_command = ToolName::namespaced("astrbot", "exec_command");
    let lookup_order = ToolName::namespaced("astrbot__live_check", "lookup_order_status");
    let chatbox = ToolName::namespaced("astrbot__vrchat", "vrchat_chatbox");
    let tool_search = ToolName::plain("tool_search");
    let memories = ToolName::namespaced("memories", "read");
    let tools = [
        exec_command.clone(),
        lookup_order.clone(),
        chatbox.clone(),
        tool_search.clone(),
        memories.clone(),
    ];
    let assigned = assign_code_mode_names(&tools, Some("astrbot"));
    assert_eq!(lookup(&assigned, &exec_command), "exec_command");
    assert_eq!(
        lookup(&assigned, &lookup_order),
        "live_check__lookup_order_status"
    );
    // Kept even where the tool's name starts with its plugin's.
    assert_eq!(lookup(&assigned, &chatbox), "vrchat__vrchat_chatbox");
    assert_eq!(lookup(&assigned, &tool_search), "tool_search");
    assert_eq!(lookup(&assigned, &memories), "memories__read");
    // A namespace that only starts with the root's letters is not the host's.
    let other = ToolName::namespaced("astrbotx", "run");
    assert_eq!(
        lookup(
            &assign_code_mode_names(std::slice::from_ref(&other), Some("astrbot")),
            &other
        ),
        "astrbotx__run"
    );
}

#[test]
fn without_a_root_names_are_unchanged() {
    let tools = [
        ToolName::namespaced("astrbot__vrchat", "vrchat_chatbox"),
        ToolName::plain("tool_search"),
    ];
    assert_eq!(
        assign_code_mode_names(&tools, None),
        assign_code_mode_names(&tools, Some(""))
    );
    assert_eq!(
        assign_code_mode_names(&tools, None)
            .into_values()
            .collect::<Vec<_>>(),
        vec![
            "astrbot__vrchat__vrchat_chatbox".to_string(),
            "tool_search".to_string()
        ]
    );
}

#[test]
fn a_plugins_namesake_tool_merges_into_one_name() {
    let qq_file = ToolName::namespaced("astrbot__qq_file", "qq_file");
    let research = ToolName::namespaced("astrbot__deepseek_research", "deepseek_research");
    let assigned = assign_code_mode_names(&[qq_file.clone(), research.clone()], Some("astrbot"));
    assert_eq!(lookup(&assigned, &qq_file), "qq_file");
    assert_eq!(lookup(&assigned, &research), "deepseek_research");
}

#[test]
fn a_namesake_does_not_merge_into_a_taken_name() {
    // Another host tool has the name.
    let qq_file = ToolName::namespaced("astrbot__qq_file", "qq_file");
    let root_qq_file = ToolName::namespaced("astrbot", "qq_file");
    let assigned =
        assign_code_mode_names(&[qq_file.clone(), root_qq_file.clone()], Some("astrbot"));
    assert_eq!(lookup(&assigned, &qq_file), "qq_file__qq_file");
    assert_eq!(lookup(&assigned, &root_qq_file), "qq_file");
    // A Codex tool has the name.
    let memories = ToolName::namespaced("astrbot__memories", "memories");
    let codex_memories = ToolName::plain("memories");
    let assigned =
        assign_code_mode_names(&[memories.clone(), codex_memories.clone()], Some("astrbot"));
    assert_eq!(lookup(&assigned, &memories), "memories__memories");
    assert_eq!(lookup(&assigned, &codex_memories), "memories");
    // A reserved name.
    let wait = ToolName::namespaced("astrbot__wait", "wait");
    assert_eq!(
        lookup(
            &assign_code_mode_names(std::slice::from_ref(&wait), Some("astrbot")),
            &wait
        ),
        "wait__wait"
    );
}

#[test]
fn a_conflict_with_a_codex_tool_prefixes_both() {
    let host_search = ToolName::namespaced("astrbot", "tool_search");
    let codex_search = ToolName::plain("tool_search");
    let host_read = ToolName::namespaced("astrbot__memories", "read");
    let codex_read = ToolName::namespaced("memories", "read");
    let host_exec = ToolName::namespaced("astrbot", "exec");
    let tools = [
        host_search.clone(),
        codex_search.clone(),
        host_read.clone(),
        codex_read.clone(),
        host_exec.clone(),
    ];
    let assigned = assign_code_mode_names(&tools, Some("astrbot"));
    assert_eq!(lookup(&assigned, &host_search), "astrbot_tool_search");
    assert_eq!(lookup(&assigned, &codex_search), "codex_tool_search");
    assert_eq!(lookup(&assigned, &host_read), "astrbot_memories__read");
    assert_eq!(lookup(&assigned, &codex_read), "codex_memories__read");
    // `exec` is code mode's own.
    assert_eq!(lookup(&assigned, &host_exec), "astrbot_exec");
    assert!(
        !assigned
            .values()
            .any(|name| name == "tool_search" || name == "memories__read" || name == "exec")
    );
}

#[test]
fn host_tools_sharing_a_name_fall_back_to_full_names() {
    // `a-b` and `a_b` normalize alike; neither keeps the bare name.
    let dashed = ToolName::namespaced("astrbot", "a-b");
    let underscored = ToolName::namespaced("astrbot", "a_b");
    let assigned = assign_code_mode_names(&[dashed.clone(), underscored.clone()], Some("astrbot"));
    assert_eq!(lookup(&assigned, &dashed), "astrbot__a_b");
    assert_eq!(lookup(&assigned, &underscored), "astrbot__a_b");
    // A prefixed name taken by another tool falls back too.
    let host = ToolName::namespaced("astrbot", "x");
    let codex_x = ToolName::plain("x");
    let literal = ToolName::plain("astrbot_x");
    let assigned = assign_code_mode_names(
        &[host.clone(), codex_x.clone(), literal.clone()],
        Some("astrbot"),
    );
    assert_eq!(lookup(&assigned, &host), "astrbot__x");
    assert_eq!(lookup(&assigned, &codex_x), "codex_x");
    assert_eq!(lookup(&assigned, &literal), "astrbot_x");
}

#[test]
fn names_do_not_depend_on_order() {
    let tools = [
        ToolName::namespaced("astrbot", "tool_search"),
        ToolName::plain("tool_search"),
        ToolName::namespaced("astrbot__qq_file", "qq_file"),
        ToolName::namespaced("astrbot", "qq_file"),
        ToolName::namespaced("astrbot__live_check", "lookup"),
        ToolName::namespaced("mcp__fs", "read"),
    ];
    let mut reversed = tools.clone();
    reversed.reverse();
    assert_eq!(
        assign_code_mode_names(&tools, Some("astrbot")),
        assign_code_mode_names(&reversed, Some("astrbot"))
    );
}
