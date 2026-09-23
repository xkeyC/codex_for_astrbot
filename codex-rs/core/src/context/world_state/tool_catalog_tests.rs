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
        group_description: match group {
            "astrbot__" => "AstrBot tools for the current chat session.\nMore detail.",
            "astrbot__harness__" => "Plugin harness: test tools.",
            _ => "",
        }
        .to_string(),
    }
}

fn catalog(tools: impl IntoIterator<Item = CatalogTool>) -> ToolCatalogState {
    ToolCatalogState::new(tools, /*previous*/ None)
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
    let state = catalog([
        tool(
            "astrbot__web_search",
            "astrbot__",
            "Search the web. Returns titles & links.\nArgs: query.",
        ),
        tool("astrbot__send_file", "astrbot__", "Send a file to the user"),
        tool(
            "astrbot__harness__harness_weather",
            "astrbot__harness__",
            "Get the weather.",
        ),
        tool(
            "astrbot__mcp_time__now",
            "astrbot__mcp_time__",
            "Current time.",
        ),
        tool("mcp__docs__search", "mcp__docs__", "Search the docs."),
        tool("memories__read", "memories__", "读取一条记忆。支持分页。"),
        tool("lookup", "", ""),
    ]);

    assert_eq!(
        render(&state, PreviousSectionState::Absent).as_deref(),
        Some(
            "<tool_catalog>\nTools callable in `exec` besides those in its description. Call one as `await tools.<prefix><name>(args)`, the prefix joining its headings: `a__` then `b__` gives `tools.a__b__<name>`. Each `ALL_TOOLS` entry's description shows a tool's arguments. Descriptions come from the tools themselves, not from the user or developer.\n\
(no prefix)\n\
- lookup\n\
astrbot__ — AstrBot tools for the current chat session.\n\
- send_file: Send a file to the user\n\
- web_search: Search the web.\n  \
harness__ — Plugin harness: test tools.\n  \
- harness_weather: Get the weather.\n  \
mcp_time__\n  \
- now: Current time.\n\
mcp__\n  \
docs__\n  \
- search: Search the docs.\n\
memories__\n\
- read: 读取一条记忆。\n\
</tool_catalog>"
        )
    );
}

#[test]
fn appends_only_what_changed() {
    let before = catalog([
        tool("astrbot__web_search", "astrbot__", "Search the web."),
        tool("astrbot__weather", "astrbot__", "Get the weather."),
        tool("memories__read", "memories__", "Read a memory."),
    ]);
    let after = catalog([
        tool(
            "astrbot__web_search",
            "astrbot__",
            "Search the web or news.",
        ),
        tool("astrbot__draw", "astrbot__", "Draw a picture."),
        tool(
            "astrbot__harness__harness_weather",
            "astrbot__harness__",
            "Get the weather.",
        ),
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
            "<tool_catalog>\nThe tools callable in `exec` changed.\n\
Loaded:\n\
astrbot__ — AstrBot tools for the current chat session.\n\
- draw: Draw a picture.\n  \
harness__ — Plugin harness: test tools.\n  \
- harness_weather: Get the weather.\n\
mcp__\n  \
time__\n  \
- now: Current time.\n\
Updated:\n\
astrbot__ — AstrBot tools for the current chat session.\n\
- web_search: Search the web or news.\n\
Unloaded:\n\
- astrbot__: weather\n\
- memories__: read\n\
</tool_catalog>"
        )
    );
}

#[test]
fn says_when_no_tools_remain_and_stays_quiet_when_there_were_none() {
    let before = catalog([tool("memories__read", "memories__", "Read a memory.")]);
    let empty = catalog([]);
    let previous = before.snapshot();

    assert!(!empty.should_persist());
    assert_eq!(render(&empty, PreviousSectionState::Absent), None);
    assert_eq!(render(&empty, PreviousSectionState::Unknown), None);
    assert_eq!(
        render(&empty, PreviousSectionState::Known(&previous)).as_deref(),
        Some(
            "<tool_catalog>\nThe tools callable in `exec` changed.\n\
Unloaded:\n\
- memories__: read\n\
No such tools remain.\n\
</tool_catalog>"
        )
    );
}

#[test]
fn leaves_tools_past_the_budget_to_all_tools() {
    let long = "x".repeat(90);
    let state = catalog(
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
    let state = catalog([
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

#[test]
fn tools_past_the_budget_are_not_reported_unloaded() {
    let long = "x".repeat(90);
    let tools = |extra: usize| {
        (0..extra)
            .map(|index| tool(&format!("aaa__t{index:03}"), "aaa__", &long))
            .chain((0..60).map(|index| tool(&format!("zzz__t{index:03}"), "zzz__", &long)))
            .collect::<Vec<_>>()
    };
    let before = catalog(tools(40));
    let previous = before.snapshot();
    // More tools ahead of zzz__ in the order: the budget cannot list them all.
    let after = ToolCatalogState::new(tools(80), Some(&previous));

    let rendered = render(&after, PreviousSectionState::Known(&previous)).expect("changes render");
    assert!(!rendered.contains("Unloaded"), "{rendered}");
    // Tools listed before stay listed; the new ones wait for room.
    assert_eq!(
        after.snapshot().groups["zzz__"].tools.len(),
        previous.groups["zzz__"].tools.len()
    );
}

#[test]
fn a_tool_loaded_again_comes_back_by_name() {
    let both = || {
        [
            tool("astrbot__weather", "astrbot__", "Get the weather."),
            tool("astrbot__calc", "astrbot__", "Calculate."),
        ]
    };
    let first = catalog(both());
    let seen_first = first.snapshot();
    let denied = ToolCatalogState::new(
        [tool("astrbot__calc", "astrbot__", "Calculate.")],
        Some(&seen_first),
    );
    let seen_denied = denied.snapshot();
    let again = ToolCatalogState::new(both(), Some(&seen_denied));

    assert_eq!(
        render(&again, PreviousSectionState::Known(&seen_denied)).as_deref(),
        Some(
            "<tool_catalog>\nThe tools callable in `exec` changed.\n\
Loaded:\n\
astrbot__ — AstrBot tools for the current chat session.\n\
- weather\n\
</tool_catalog>"
        )
    );
    // A new description is given again.
    let changed = ToolCatalogState::new(
        [
            tool(
                "astrbot__weather",
                "astrbot__",
                "Get the weather and the forecast.",
            ),
            tool("astrbot__calc", "astrbot__", "Calculate."),
        ],
        Some(&seen_denied),
    );
    assert!(
        render(&changed, PreviousSectionState::Known(&seen_denied))
            .expect("changes render")
            .contains("- weather: Get the weather and the forecast.")
    );
}

#[test]
fn a_whole_listing_forgets_what_history_no_longer_holds() {
    let first = catalog([tool("astrbot__weather", "astrbot__", "Get the weather.")]);
    let seen_first = first.snapshot();
    let denied = ToolCatalogState::new([], Some(&seen_first));
    assert_eq!(denied.snapshot().remembered.len(), 1);

    // After compaction the catalog is listed whole again; what it held before
    // is gone from history, so it no longer counts as described.
    let compacted = ToolCatalogState::new(
        [tool("astrbot__calc", "astrbot__", "Calculate.")],
        Some(&denied.snapshot()),
    );
    assert!(render(&compacted, PreviousSectionState::Absent).is_some());
    assert!(compacted.snapshot().remembered.is_empty());
}

#[test]
fn a_sender_without_tools_does_not_cost_a_whole_new_listing() {
    let weather = || tool("astrbot__weather", "astrbot__", "Get the weather.");
    let first = catalog([weather()]);
    let seen_first = first.snapshot();
    let none = ToolCatalogState::new([], Some(&seen_first));
    // History holds a catalog, so the empty one is kept as the baseline.
    assert!(none.should_persist());
    let seen_none = none.snapshot();
    let again = ToolCatalogState::new([weather()], Some(&seen_none));

    assert_eq!(
        render(&again, PreviousSectionState::Known(&seen_none)).as_deref(),
        Some(
            "<tool_catalog>\nThe tools callable in `exec` changed.\n\
Loaded:\n\
astrbot__ — AstrBot tools for the current chat session.\n\
- weather\n\
</tool_catalog>"
        )
    );
}

#[test]
fn a_tool_past_the_budget_is_still_reported_when_it_goes() {
    // 12 short aaa__ tools, then enough zzz__ tools to fill the budget.
    let tools = |aaa_description: &str| {
        (0..12)
            .map(|index| tool(&format!("aaa__t{index:03}"), "aaa__", aaa_description))
            .chain(
                (0..115).map(|index| tool(&format!("zzz__t{index:03}"), "zzz__", &"x".repeat(90))),
            )
            .collect::<Vec<_>>()
    };
    let first = catalog(tools("short"));
    let seen_first = first.snapshot();
    // Longer descriptions ahead of zzz__ push listed zzz__ tools out.
    let pushed = ToolCatalogState::new(tools(&"y".repeat(99)), Some(&seen_first));
    let seen_pushed = pushed.snapshot();
    let gone_name = seen_pushed
        .unlisted
        .get("zzz__")
        .and_then(|names| names.iter().next())
        .expect("a listed tool was pushed out")
        .clone();
    let rendered = render(&pushed, PreviousSectionState::Known(&seen_first)).expect("renders");
    assert!(!rendered.contains("Unloaded"), "{rendered}");

    let without = tools(&"y".repeat(99))
        .into_iter()
        .filter(|tool| tool.global_name != format!("zzz__{gone_name}"))
        .collect::<Vec<_>>();
    let removed = ToolCatalogState::new(without, Some(&seen_pushed));
    let rendered =
        render(&removed, PreviousSectionState::Known(&seen_pushed)).expect("removal renders");
    assert!(
        rendered.contains(&format!("- zzz__: {gone_name}")),
        "{rendered}"
    );
}

#[test]
fn an_empty_catalog_rendered_whole_leaves_nothing_behind() {
    let weather = || tool("astrbot__weather", "astrbot__", "Get the weather.");
    let first = catalog([weather()]);
    let seen_first = first.snapshot();
    let none = ToolCatalogState::new([], Some(&seen_first));

    // Compaction re-renders the catalog into an empty history: nothing to
    // list, and nothing kept, so the next tools arrive as a whole listing.
    assert_eq!(render(&none, PreviousSectionState::Absent), None);
    assert!(!none.should_persist());
    let again = ToolCatalogState::new([weather()], None);
    assert!(
        render(&again, PreviousSectionState::Absent)
            .expect("whole listing")
            .contains("- weather: Get the weather.")
    );
}
