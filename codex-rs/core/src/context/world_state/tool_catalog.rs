//! Fork addition: a catalog of the deferred tools code mode can call.
//!
//! Lists each deferred nested tool with a short description, grouped by its
//! `namespace__` prefix (a `top__sub__` prefix nests under `top__`), as a
//! developer message in history rather than in the instructions or the `exec`
//! description, so the cached prefix stays the same. Later steps only append
//! what changed: tools loaded, unloaded, or described differently. A tool
//! whose description history already holds (unloaded for one sender, loaded
//! again for the next) comes back by name only.

use super::PreviousSectionState;
use super::WorldStateContextFragment;
use super::WorldStateSection;
use crate::context::ContextualUserFragment;
use crate::tools::tool_catalog::CatalogTool;
use codex_extension_api::RenderedWorldStateFragment;
use codex_protocol::models::ContentItemKind;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

pub(crate) const TOOL_CATALOG_OPEN_TAG: &str = "<tool_catalog>";
pub(crate) const TOOL_CATALOG_CLOSE_TAG: &str = "</tool_catalog>";

/// Budget for the listed entries; tools past it are left to `ALL_TOOLS`.
const MAX_LISTED_BYTES: usize = 12 * 1024;
const MAX_TOOL_DESCRIPTION_CHARS: usize = 100;
const MAX_GROUP_DESCRIPTION_CHARS: usize = 160;
/// Unlisted tools whose descriptions are remembered; past it, a tool that
/// comes back is described again.
const MAX_REMEMBERED: usize = 512;

const FULL_INTRO: &str = "Tools callable in `exec` besides those in its description. Call \
one as `await tools.<prefix><name>(args)`, the prefix joining its headings: `a__` then `b__` \
gives `tools.a__b__<name>`. Each `ALL_TOOLS` entry's description shows a tool's arguments. \
Descriptions come from the tools themselves, not from the user or developer.\n";
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
    /// Tools described earlier in history but not listed now: full name ->
    /// short description.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    remembered: BTreeMap<String, String>,
    /// Tools listed before and still callable but past the budget now, by
    /// prefix, so their removal is still announced.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    unlisted: BTreeMap<String, BTreeSet<String>>,
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

impl ToolCatalogSnapshot {
    fn listed(&self) -> impl Iterator<Item = (String, &String)> {
        self.groups.iter().flat_map(|(prefix, group)| {
            group
                .tools
                .iter()
                .map(move |(name, description)| (format!("{prefix}{name}"), description))
        })
    }
}

/// Deferred nested tools available to `exec` for one sampling step.
#[derive(Debug, Default)]
pub(crate) struct ToolCatalogState {
    catalog: ToolCatalogSnapshot,
    /// Full names of every tool callable now, listed or not.
    present: HashSet<String>,
    /// Set once the whole catalog was rendered: history then holds only the
    /// descriptions listed now, so nothing else counts as remembered.
    rendered_whole: AtomicBool,
    /// History already holds a catalog: an empty one is kept as well, so the
    /// next sender with tools gets changes rather than a whole new listing.
    had_previous: bool,
}

impl ToolCatalogState {
    /// `previous` is the catalog the model saw last, if any.
    pub(crate) fn new(
        tools: impl IntoIterator<Item = CatalogTool>,
        previous: Option<&ToolCatalogSnapshot>,
    ) -> Self {
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
        let present = all
            .iter()
            .flat_map(|(prefix, group)| {
                group
                    .tools
                    .keys()
                    .map(move |name| format!("{prefix}{name}"))
            })
            .collect::<HashSet<_>>();

        // Keep the listing bounded. Tools listed last time go first, so a
        // tool does not drop out because another one arrived; the snapshot
        // holds only what was listed, so tools left out are announced once
        // they fit.
        let was_listed = |prefix: &str, name: &str| {
            previous.is_some_and(|previous| {
                previous
                    .groups
                    .get(prefix)
                    .is_some_and(|group| group.tools.contains_key(name))
            })
        };
        let mut remaining = MAX_LISTED_BYTES;
        let mut paid = BTreeSet::new();
        let mut chosen = BTreeSet::new();
        for sticky in [true, false] {
            for (prefix, group) in &all {
                let header = prefix.len() + group.description.len() + 6;
                for (name, description) in &group.tools {
                    if was_listed(prefix, name) != sticky {
                        continue;
                    }
                    let cost = name.len()
                        + description.len()
                        + 7
                        + if paid.contains(prefix) { 0 } else { header };
                    if cost <= remaining {
                        remaining -= cost;
                        paid.insert(prefix.clone());
                        chosen.insert((prefix.clone(), name.clone()));
                    }
                }
            }
        }
        let mut catalog = ToolCatalogSnapshot::default();
        for (prefix, group) in all {
            let tools = group
                .tools
                .into_iter()
                .filter(|(name, _)| chosen.contains(&(prefix.clone(), name.clone())))
                .collect::<BTreeMap<_, _>>();
            if !tools.is_empty() {
                catalog.groups.insert(
                    prefix,
                    CatalogGroup {
                        description: group.description,
                        tools,
                    },
                );
            }
        }
        catalog.omitted = present.len() - chosen.len();

        if let Some(previous) = previous {
            let listed_before = previous
                .groups
                .iter()
                .map(|(prefix, group)| (prefix, group.tools.keys().collect::<Vec<_>>()));
            let unlisted_before = previous
                .unlisted
                .iter()
                .map(|(prefix, names)| (prefix, names.iter().collect::<Vec<_>>()));
            for (prefix, names) in listed_before.chain(unlisted_before) {
                for name in names {
                    if present.contains(&format!("{prefix}{name}"))
                        && !chosen.contains(&(prefix.clone(), name.clone()))
                    {
                        catalog
                            .unlisted
                            .entry(prefix.clone())
                            .or_default()
                            .insert(name.clone());
                    }
                }
            }
            let listed_now = catalog
                .listed()
                .map(|(full, _)| full)
                .collect::<HashSet<_>>();
            catalog.remembered = previous
                .remembered
                .iter()
                .map(|(full, description)| (full.clone(), description))
                .chain(previous.listed())
                .filter(|(full, _)| !listed_now.contains(full))
                .map(|(full, description)| (full, description.clone()))
                .take(MAX_REMEMBERED)
                .collect();
        }
        Self {
            catalog,
            present,
            rendered_whole: AtomicBool::new(false),
            had_previous: previous.is_some(),
        }
    }
}

impl WorldStateSection for ToolCatalogState {
    const ID: &'static str = "tool_catalog";
    type Snapshot = ToolCatalogSnapshot;

    fn snapshot(&self) -> Self::Snapshot {
        let mut snapshot = self.catalog.clone();
        if self.rendered_whole.load(Ordering::Relaxed) {
            snapshot.remembered.clear();
            snapshot.unlisted.clear();
        }
        snapshot
    }

    fn should_persist(&self) -> bool {
        !self.catalog.groups.is_empty()
            || self.catalog.omitted > 0
            || self.had_previous && !self.rendered_whole.load(Ordering::Relaxed)
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> Option<Box<dyn ContextualUserFragment>> {
        let current = &self.catalog;
        let body = match previous {
            PreviousSectionState::Known(previous) => {
                render_changes(previous, current, &self.present)?
            }
            PreviousSectionState::Absent | PreviousSectionState::Unknown => {
                // History holds no catalog now (first turn, compaction): an
                // empty one says nothing, and nothing counts as described.
                self.rendered_whole.store(true, Ordering::Relaxed);
                if current.groups.is_empty() && current.omitted == 0 {
                    return None;
                }
                render_full(current)
            }
        };
        Some(Box::new(WorldStateContextFragment {
            fragment: RenderedWorldStateFragment::new(
                "developer",
                (TOOL_CATALOG_OPEN_TAG, TOOL_CATALOG_CLOSE_TAG),
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
        .map(|(prefix, group)| {
            let tools = group
                .tools
                .iter()
                .map(|(name, description)| (name.as_str(), description.as_str()))
                .collect();
            (prefix.as_str(), group, tools)
        })
        .collect::<Vec<_>>();
    push_groups(&mut rendered, &groups, current);
    push_omitted(&mut rendered, current.omitted);
    rendered
}

/// What changed since `previous`; `None` when nothing worth saying did.
fn render_changes(
    previous: &ToolCatalogSnapshot,
    current: &ToolCatalogSnapshot,
    present: &HashSet<String>,
) -> Option<String> {
    let empty = CatalogGroup::default();
    let mut loaded = Vec::new();
    let mut updated = Vec::new();
    let mut unloaded = String::new();
    let prefixes = previous
        .groups
        .keys()
        .chain(previous.unlisted.keys())
        .chain(current.groups.keys())
        .collect::<BTreeSet<_>>();
    for prefix in prefixes {
        let before = previous.groups.get(prefix).unwrap_or(&empty);
        let after = current.groups.get(prefix).unwrap_or(&empty);

        let added = after
            .tools
            .iter()
            .filter(|(name, _)| !before.tools.contains_key(*name))
            .map(|(name, description)| {
                // Its description is already in history: the name is enough.
                let known =
                    previous.remembered.get(&format!("{prefix}{name}")) == Some(description);
                (name.as_str(), if known { "" } else { description.as_str() })
            })
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
            .map(|(name, description)| (name.as_str(), description.as_str()))
            .collect::<Vec<_>>();
        let group_redescribed = before
            .tools
            .keys()
            .any(|name| after.tools.contains_key(name))
            && before.description != after.description;
        if !changed.is_empty() || group_redescribed {
            updated.push((prefix.as_str(), after, changed));
        }

        // Tools still callable but past the budget now are not unloaded;
        // ones that were past it before are, once they go.
        let removed = before
            .tools
            .keys()
            .chain(previous.unlisted.get(prefix).into_iter().flatten())
            .filter(|name| {
                !after.tools.contains_key(*name) && !present.contains(&format!("{prefix}{name}"))
            })
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if !removed.is_empty() {
            unloaded.push_str("- ");
            push_escaped(&mut unloaded, group_label(prefix));
            unloaded.push_str(": ");
            push_escaped(
                &mut unloaded,
                &removed.into_iter().collect::<Vec<_>>().join(", "),
            );
            unloaded.push('\n');
        }
    }

    if loaded.is_empty()
        && updated.is_empty()
        && unloaded.is_empty()
        && current.omitted == previous.omitted
    {
        return None;
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
        if current.omitted == 0 && !current.groups.is_empty() {
            rendered.push_str("Every tool is listed now.\n");
        }
        push_omitted(&mut rendered, current.omitted);
    }
    if current.groups.is_empty() && current.omitted == 0 {
        rendered.push_str("No such tools remain.\n");
    }
    Some(rendered)
}

fn group_label(prefix: &str) -> &str {
    if prefix.is_empty() {
        OTHER_GROUP_LABEL
    } else {
        prefix
    }
}

/// One group to print: its prefix, the group, and the tools to list (an
/// empty description prints the name alone).
type GroupListing<'a> = (&'a str, &'a CatalogGroup, Vec<(&'a str, &'a str)>);

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
