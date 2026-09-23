use super::ToolCatalogSnapshot;
use super::ToolCatalogState;
use super::short_description;
use crate::context::world_state::PreviousSectionState;
use crate::context::world_state::WorldStateSection;
use crate::tools::tool_catalog::CatalogTool;
use pretty_assertions::assert_eq;

fn tool(global_name: &str, group: &str, description: &str) -> CatalogTool {
    CatalogTool {
        global_name: global_name.to_string(),
        group: group.to_string(),
        description: description.to_string(),
        group_description: if group == "astrbot__" {
            "AstrBot plugin tools for the current chat session.\nMore detail.".to_string()
        } else {
            String::new()
        },
    }
}

fn render(
    state: &ToolCatalogState,
    previous: PreviousSectionState<'_, ToolCatalogSnapshot>,
) -> Option<String> {
    state
        .render_diff(previous)
        .map(|fragment| fragment.render())
}

#[test]
fn lists_tools_by_prefix_with_short_descriptions() {
    let state = ToolCatalogState::new([
        tool(
            "astrbot__web_search",
            "astrbot__",
            "Search the web. Returns titles & links.\nArgs: query.",
        ),
        tool("astrbot__send_file", "astrbot__", "Send a file to the user"),
        tool("memories__read", "memories__", "读取一条记忆。支持分页。"),
        tool("lookup", "", ""),
    ]);

    assert_eq!(
        render(&state, PreviousSectionState::Absent).as_deref(),
        Some(
            "<tools>\nTools callable in `exec` besides those in its description, as `await tools.<prefix><name>(args)`. Each `ALL_TOOLS` entry's description shows a tool's arguments.\n\
(no prefix)\n\
- lookup\n\
astrbot__ — AstrBot plugin tools for the current chat session.\n\
- send_file: Send a file to the user\n\
- web_search: Search the web.\n\
memories__\n\
- read: 读取一条记忆。\n\
</tools>"
        )
    );
}

#[test]
fn appends_only_what_changed() {
    let before = ToolCatalogState::new([
        tool("astrbot__web_search", "astrbot__", "Search the web."),
        tool("astrbot__weather", "astrbot__", "Get the weather."),
        tool("memories__read", "memories__", "Read a memory."),
    ]);
    let after = ToolCatalogState::new([
        tool(
            "astrbot__web_search",
            "astrbot__",
            "Search the web or news.",
        ),
        tool("astrbot__draw", "astrbot__", "Draw a picture."),
        tool("mcp__time__now", "mcp__time__", "Current time."),
    ]);
    let previous = before.snapshot();

    assert_eq!(
        render(&before, PreviousSectionState::Known(&previous)),
        None
    );
    assert_eq!(
        render(&after, PreviousSectionState::Known(&previous)).as_deref(),
        Some(
            "<tools>\nThe tools callable in `exec` changed.\n\
Loaded:\n\
astrbot__ — AstrBot plugin tools for the current chat session.\n\
- draw: Draw a picture.\n\
mcp__time__\n\
- now: Current time.\n\
Updated:\n\
astrbot__ — AstrBot plugin tools for the current chat session.\n\
- web_search: Search the web or news.\n\
Unloaded:\n\
- astrbot__: weather\n\
- memories__: read\n\
</tools>"
        )
    );
}

#[test]
fn says_when_no_tools_remain_and_stays_quiet_when_there_were_none() {
    let before = ToolCatalogState::new([tool("memories__read", "memories__", "Read a memory.")]);
    let empty = ToolCatalogState::new([]);
    let previous = before.snapshot();

    assert!(!empty.should_persist());
    assert_eq!(render(&empty, PreviousSectionState::Absent), None);
    assert_eq!(render(&empty, PreviousSectionState::Unknown), None);
    assert_eq!(
        render(&empty, PreviousSectionState::Known(&previous)).as_deref(),
        Some(
            "<tools>\nThe tools callable in `exec` changed.\n\
Unloaded:\n\
- memories__: read\n\
No such tools remain.\n\
</tools>"
        )
    );
}

#[test]
fn leaves_tools_past_the_budget_to_all_tools() {
    let long = "x".repeat(90);
    let state = ToolCatalogState::new(
        (0..400).map(|index| tool(&format!("astrbot__tool_{index:03}"), "astrbot__", &long)),
    );
    let snapshot = state.snapshot();
    let listed = snapshot.groups["astrbot__"].tools.len();

    assert!(listed > 0 && listed < 400);
    assert_eq!(snapshot.omitted, 400 - listed);
    let rendered = render(&state, PreviousSectionState::Absent).expect("catalog renders");
    assert!(rendered.len() < 14 * 1024);
    assert!(rendered.contains(&format!(
        "{} more not listed; filter `ALL_TOOLS` by name to find them.",
        400 - listed
    )));
}

#[test]
fn snapshots_round_trip_through_json() {
    let state = ToolCatalogState::new([
        tool("astrbot__web_search", "astrbot__", "Search the web."),
        tool("lookup", "", "Look it up."),
    ]);
    let json = serde_json::to_value(state.snapshot()).expect("snapshot serializes");
    let restored: ToolCatalogSnapshot = serde_json::from_value(json).expect("snapshot restores");

    assert_eq!(restored, state.snapshot());
    assert_eq!(render(&state, PreviousSectionState::Known(&restored)), None);
}

#[test]
fn shortens_descriptions_to_their_first_sentence() {
    assert_eq!(
        short_description("  \n Fetch a page. Then more.", 100),
        "Fetch a page."
    );
    assert_eq!(
        short_description("v1.2 of the tool is here", 100),
        "v1.2 of the tool is here"
    );
    assert_eq!(short_description("发送消息！然后结束", 100), "发送消息！");
    assert_eq!(
        short_description(
            "Read a skill's file (relative `path`, e.g. docs/a.md). More.",
            100
        ),
        "Read a skill's file (relative `path`, e.g. docs/a.md)."
    );
    assert_eq!(
        short_description("Tabs, links etc. are kept. Rest.", 100),
        "Tabs, links etc. are kept."
    );
    assert_eq!(
        short_description(&"a".repeat(120), 10),
        format!("{}…", "a".repeat(9))
    );
}
