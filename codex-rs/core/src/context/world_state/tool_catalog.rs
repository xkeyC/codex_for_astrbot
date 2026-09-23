//! Fork addition: a catalog of the deferred tools code mode can call.
//!
//! Lists each deferred nested tool with a short description, grouped by its
//! `namespace__` prefix (a `top__sub__` prefix nests under `top__`), as a
//! developer message in history rather than in the
//! instructions or the `exec` description, so the cached prefix stays the
//! same. Later steps only append what changed: tools loaded, unloaded, or
//! described differently.

use super::PreviousSectionState;
use super::WorldStateContextFragment;
use super::WorldStateSection;
use crate::context::ContextualUserFragment;
use crate::tools::tool_catalog::CatalogTool;
use codex_extension_api::RenderedWorldStateFragment;
use codex_protocol::models::ContentItemKind;
use codex_protocol::protocol::TOOLS_CLOSE_TAG;
use codex_protocol::protocol::TOOLS_OPEN_TAG;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

/// Budget for the listed entries; tools past it are left to `ALL_TOOLS`.
const MAX_LISTED_BYTES: usize = 12 * 1024;
const MAX_TOOL_DESCRIPTION_CHARS: usize = 100;
const MAX_GROUP_DESCRIPTION_CHARS: usize = 160;

const FULL_INTRO: &str = "Tools callable in `exec` besides those in its description. Call \
one as `await tools.<prefix><name>(args)`, the prefix joining its headings: `a__` then `b__` \
gives `tools.a__b__<name>`. Each `ALL_TOOLS` entry's description shows a tool's arguments.\n";
const DIFF_INTRO: &str = "The tools callable in `exec` changed.\n";
const OTHER_GROUP_LABEL: &str = "(no prefix)";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolCatalogSnapshot {
    /// Keyed by `namespace__` prefix; the empty key holds plain tools.
    #[serde(default)]
    groups: BTreeMap<String, CatalogGroup>,
    /// Tools past the listing budget.
    #[serde(default, skip_serializing_if = "is_zero")]
    omitted: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct CatalogGroup {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,
    /// Name after the group prefix -> short description.
    #[serde(default)]
    tools: BTreeMap<String, String>,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes skipped fields by reference"
)]
fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Deferred nested tools available to `exec` for one sampling step.
#[derive(Debug, Default)]
pub(crate) struct ToolCatalogState {
    catalog: ToolCatalogSnapshot,
}

impl ToolCatalogState {
    pub(crate) fn new(tools: impl IntoIterator<Item = CatalogTool>) -> Self {
        let mut all = BTreeMap::<String, CatalogGroup>::new();
        for tool in tools {
            let name = tool.global_name[tool.group.len()..].to_string();
            let group = all.entry(tool.group).or_default();
            if group.description.is_empty() {
                group.description =
                    short_description(&tool.group_description, MAX_GROUP_DESCRIPTION_CHARS);
            }
            group.tools.insert(
                name,
                short_description(&tool.description, MAX_TOOL_DESCRIPTION_CHARS),
            );
        }

        // Keep the listing bounded; the snapshot holds only what was listed,
        // so tools left out now are announced once they fit.
        let mut catalog = ToolCatalogSnapshot::default();
        let mut remaining = MAX_LISTED_BYTES;
        for (prefix, group) in all {
            let header = prefix.len() + group.description.len() + 4;
            let mut listed = CatalogGroup {
                description: group.description,
                tools: BTreeMap::new(),
            };
            let mut header_paid = false;
            for (name, description) in group.tools {
                let entry = name.len() + description.len() + 5;
                let cost = entry + if header_paid { 0 } else { header };
                if cost <= remaining {
                    remaining -= cost;
                    header_paid = true;
                    listed.tools.insert(name, description);
                } else {
                    catalog.omitted += 1;
                }
            }
            if !listed.tools.is_empty() {
                catalog.groups.insert(prefix, listed);
            }
        }
        Self { catalog }
    }
}

impl WorldStateSection for ToolCatalogState {
    const ID: &'static str = "tool_catalog";
    type Snapshot = ToolCatalogSnapshot;

    fn snapshot(&self) -> Self::Snapshot {
        self.catalog.clone()
    }

    fn should_persist(&self) -> bool {
        !self.catalog.groups.is_empty() || self.catalog.omitted > 0
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> Option<Box<dyn ContextualUserFragment>> {
        let current = &self.catalog;
        let body = match previous {
            PreviousSectionState::Known(previous) if previous == current => return None,
            PreviousSectionState::Known(previous) => render_changes(previous, current),
            PreviousSectionState::Absent | PreviousSectionState::Unknown => {
                if !self.should_persist() {
                    return None;
                }
                render_full(current)
            }
        };
        Some(Box::new(WorldStateContextFragment {
            fragment: RenderedWorldStateFragment::new(
                "developer",
                (TOOLS_OPEN_TAG, TOOLS_CLOSE_TAG),
                body,
            ),
            content_kind: ContentItemKind("tools.catalog".to_string()),
        }))
    }
}

fn render_full(current: &ToolCatalogSnapshot) -> String {
    let mut rendered = format!("\n{FULL_INTRO}");
    let groups = current
        .groups
        .iter()
        .map(|(prefix, group)| (prefix.as_str(), group, group.tools.iter().collect()))
        .collect::<Vec<_>>();
    push_groups(&mut rendered, &groups, current);
    push_omitted(&mut rendered, current.omitted);
    rendered
}

fn render_changes(previous: &ToolCatalogSnapshot, current: &ToolCatalogSnapshot) -> String {
    let empty = CatalogGroup::default();
    let mut loaded = Vec::new();
    let mut updated = Vec::new();
    let mut unloaded = String::new();
    let prefixes = previous
        .groups
        .keys()
        .chain(current.groups.keys())
        .collect::<std::collections::BTreeSet<_>>();
    for prefix in prefixes {
        let before = previous.groups.get(prefix).unwrap_or(&empty);
        let after = current.groups.get(prefix).unwrap_or(&empty);

        let added = after
            .tools
            .iter()
            .filter(|(name, _)| !before.tools.contains_key(*name))
            .collect::<Vec<_>>();
        if !added.is_empty() {
            loaded.push((prefix.as_str(), after, added));
        }

        let changed = after
            .tools
            .iter()
            .filter(|(name, description)| {
                before
                    .tools
                    .get(*name)
                    .is_some_and(|previous| previous != *description)
            })
            .collect::<Vec<_>>();
        let group_redescribed = before
            .tools
            .keys()
            .any(|name| after.tools.contains_key(name))
            && before.description != after.description;
        if !changed.is_empty() || group_redescribed {
            updated.push((prefix.as_str(), after, changed));
        }

        let removed = before
            .tools
            .keys()
            .filter(|name| !after.tools.contains_key(*name))
            .map(String::as_str)
            .collect::<Vec<_>>();
        if !removed.is_empty() {
            unloaded.push_str("- ");
            push_escaped(&mut unloaded, group_label(prefix));
            unloaded.push_str(": ");
            push_escaped(&mut unloaded, &removed.join(", "));
            unloaded.push('\n');
        }
    }

    let mut rendered = format!("\n{DIFF_INTRO}");
    for (label, groups) in [("Loaded", loaded), ("Updated", updated)] {
        if !groups.is_empty() {
            rendered.push_str(label);
            rendered.push_str(":\n");
            push_groups(&mut rendered, &groups, current);
        }
    }
    if !unloaded.is_empty() {
        rendered.push_str("Unloaded:\n");
        rendered.push_str(&unloaded);
    }
    if current.omitted != previous.omitted {
        push_omitted(&mut rendered, current.omitted);
    }
    if current.groups.is_empty() && current.omitted == 0 {
        rendered.push_str("No such tools remain.\n");
    }
    rendered
}

fn group_label(prefix: &str) -> &str {
    if prefix.is_empty() {
        OTHER_GROUP_LABEL
    } else {
        prefix
    }
}

/// One group to print: its prefix, the group, and the tools to list.
type GroupListing<'a> = (&'a str, &'a CatalogGroup, Vec<(&'a String, &'a String)>);

/// Prints groups in prefix order, nesting `top__sub__` under a `top__`
/// heading; a heading takes the description of the `top__` group itself.
fn push_groups(rendered: &mut String, groups: &[GroupListing<'_>], current: &ToolCatalogSnapshot) {
    let mut current_top = None;
    for (prefix, group, tools) in groups {
        let (top, sub) = split_prefix(prefix);
        if current_top != Some(top) {
            current_top = Some(top);
            let description = current
                .groups
                .get(top)
                .map_or("", |group| group.description.as_str());
            push_heading(rendered, "", group_label(top), description);
        }
        let indent = if sub.is_empty() {
            ""
        } else {
            push_heading(rendered, "  ", sub, &group.description);
            "  "
        };
        for (name, description) in tools {
            push_tool(rendered, indent, name, description);
        }
    }
}

/// Splits `top__rest` after the first `__`; a prefix without one is all top.
fn split_prefix(prefix: &str) -> (&str, &str) {
    prefix
        .find("__")
        .map_or((prefix, ""), |index| prefix.split_at(index + 2))
}

fn push_heading(rendered: &mut String, indent: &str, label: &str, description: &str) {
    rendered.push_str(indent);
    push_escaped(rendered, label);
    if !description.is_empty() {
        rendered.push_str(" — ");
        push_escaped(rendered, description);
    }
    rendered.push('\n');
}

fn push_tool(rendered: &mut String, indent: &str, name: &str, description: &str) {
    rendered.push_str(indent);
    rendered.push_str("- ");
    push_escaped(rendered, name);
    if !description.is_empty() {
        rendered.push_str(": ");
        push_escaped(rendered, description);
    }
    rendered.push('\n');
}

fn push_omitted(rendered: &mut String, omitted: usize) {
    if omitted > 0 {
        rendered.push_str(&format!(
            "{omitted} more not listed; filter `ALL_TOOLS` by name to find them.\n"
        ));
    }
}

/// Escapes only what could end or open a tag; quotes stay readable.
fn push_escaped(rendered: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => rendered.push_str("&amp;"),
            '<' => rendered.push_str("&lt;"),
            '>' => rendered.push_str("&gt;"),
            _ => rendered.push(ch),
        }
    }
}

/// First sentence of the first non-empty line, at most `max_chars` long.
fn short_description(text: &str, max_chars: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let sentence = sentence_end(line).map_or(line, |end| &line[..end]);
    if sentence.chars().count() <= max_chars {
        return sentence.to_string();
    }
    let mut short = sentence
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    short.truncate(short.trim_end().len());
    short.push('…');
    short
}

/// Byte offset just past the first sentence's end mark.
fn sentence_end(line: &str) -> Option<usize> {
    let mut chars = line.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let end = index + ch.len_utf8();
        match ch {
            '。' | '！' | '？' => return Some(end),
            '.' | '!' | '?'
                if chars.peek().is_some_and(|(_, next)| next.is_whitespace())
                    && !(ch == '.' && is_abbreviation(&line[..index])) =>
            {
                return Some(end);
            }
            _ => {}
        }
    }
    None
}

/// Whether the word before a period is an abbreviation (`e.g`, `etc`).
fn is_abbreviation(before: &str) -> bool {
    let word = before
        .rsplit(|ch: char| ch.is_whitespace() || ch == '(')
        .next()
        .unwrap_or_default();
    let initials = word.split('.').collect::<Vec<_>>();
    initials.len() > 1
        && initials
            .iter()
            .all(|initial| initial.chars().count() == 1 && initial.chars().all(char::is_alphabetic))
        || ["etc", "vs", "eg", "ie", "approx", "incl"]
            .iter()
            .any(|abbreviation| word.eq_ignore_ascii_case(abbreviation))
}

#[cfg(test)]
#[path = "tool_catalog_tests.rs"]
mod tests;
