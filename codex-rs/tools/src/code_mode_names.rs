//! Fork addition: the names nested tools get on code mode's `tools` object
//! when a host namespace root is configured
//! (`features.code_mode.host_namespace`).
//!
//! The host's tools are those in the root's namespace (`astrbot`) and in
//! plugin namespaces under it (`astrbot__<plugin>`). The root is dropped:
//!
//! 1. A tool of the root namespace goes by its own name: `astrbot` +
//!    `exec_command` is `exec_command`.
//! 2. A plugin's tool keeps the plugin layer: `astrbot__live_check` +
//!    `lookup_order_status` is `live_check__lookup_order_status`,
//!    `astrbot__vrchat` + `vrchat_chatbox` is `vrchat__vrchat_chatbox`.
//! 3. A plugin tool named exactly like its plugin merges into that name
//!    (`astrbot__qq_file` + `qq_file` is `qq_file`), unless another tool
//!    would have the same name (or it is `exec`, `wait`, `run_code`); then
//!    it stays `qq_file__qq_file`.
//! 4. A host name equal to another tool's name (or reserved): neither keeps
//!    it. The host tool becomes `<root>_<name>` and the other tool
//!    `codex_<its name>`.
//!
//! A name that is still not unique falls back to the tool's full code-mode
//! name. Names depend only on the whole tool set, not its order, so a stable
//! set keeps stable names (and the prompt cache). Without a root every tool
//! keeps its full code-mode name.

use std::collections::BTreeMap;

use crate::ToolName;
use crate::code_mode_name_for_tool_name;

/// Final code-mode names, keyed by `ToolName::with_default_namespace`.
pub type CodeModeNames = BTreeMap<ToolName, String>;

/// Names reserved by code mode itself.
const RESERVED: [&str; 3] = [
    codex_code_mode::PUBLIC_TOOL_NAME,
    codex_code_mode::WAIT_TOOL_NAME,
    codex_code_mode::RUN_CODE_TOOL_NAME,
];

/// A host tool's place under the root.
#[derive(Clone)]
struct HostTool {
    /// The tool's own name, normalized.
    name: String,
    /// The plugin namespace without the root, normalized; `None` for the
    /// root namespace itself.
    plugin: Option<String>,
}

impl HostTool {
    /// Its name by rules 1 and 2.
    fn layered(&self) -> String {
        match &self.plugin {
            Some(plugin) => format!("{plugin}__{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Its name by rule 3, when it is a plugin's namesake.
    fn merged(&self) -> Option<&str> {
        (self.plugin.as_deref() == Some(self.name.as_str())).then_some(self.name.as_str())
    }
}

/// Assigns each nested tool its name on the `tools` object.
pub fn assign_code_mode_names<'a>(
    tools: impl IntoIterator<Item = &'a ToolName>,
    host_namespace: Option<&str>,
) -> CodeModeNames {
    let root = host_namespace.filter(|root| !root.is_empty());
    let entries = tools
        .into_iter()
        .map(|tool_name| {
            let key = tool_name.clone().with_default_namespace();
            let full = codex_code_mode::normalize_code_mode_identifier(
                &code_mode_name_for_tool_name(tool_name),
            );
            let host = root.and_then(|root| host_tool(tool_name, root));
            (key, (full, host))
        })
        .collect::<BTreeMap<_, _>>();
    let Some(root) = root else {
        return entries
            .into_iter()
            .map(|(key, (full, _))| (key, full))
            .collect();
    };

    // Rules 1-3: a namesake merges only into a name no other tool has.
    let tentative = |(full, host): &(String, Option<HostTool>)| match host {
        Some(host) => host.merged().map_or_else(|| host.layered(), str::to_string),
        None => full.clone(),
    };
    let tentative_uses = count(entries.values().map(tentative));
    let candidates = entries
        .iter()
        .map(|(key, entry)| {
            let candidate = match &entry.1 {
                Some(host) => match host.merged() {
                    Some(merged) if tentative_uses[merged] == 1 && !RESERVED.contains(&merged) => {
                        merged.to_string()
                    }
                    _ => host.layered(),
                },
                None => entry.0.clone(),
            };
            (key.clone(), candidate)
        })
        .collect::<BTreeMap<_, _>>();

    // Rule 4: a name shared with another tool, or reserved, is left to none.
    let uses = count(candidates.values().cloned());
    let mut names = candidates
        .iter()
        .map(|(key, candidate)| {
            let shared = uses[candidate] > 1 || RESERVED.contains(&candidate.as_str());
            let name = match (&entries[key].1, shared) {
                (_, false) => candidate.clone(),
                (Some(_), true) => format!("{root}_{candidate}"),
                (None, true) => format!("codex_{candidate}"),
            };
            (key.clone(), name)
        })
        .collect::<CodeModeNames>();

    // A name still taken twice falls back to the full code-mode names.
    let uses = count(names.values().cloned());
    let clashing = names
        .iter()
        .filter(|(_, name)| uses[*name] > 1)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in clashing {
        names.insert(key.clone(), entries[&key].0.clone());
    }
    names
}

fn count(names: impl Iterator<Item = String>) -> BTreeMap<String, usize> {
    let mut uses = BTreeMap::new();
    for name in names {
        *uses.entry(name).or_default() += 1;
    }
    uses
}

/// Where a tool sits under the host root, if it does.
fn host_tool(tool_name: &ToolName, root: &str) -> Option<HostTool> {
    if tool_name.is_default_namespace() {
        return None;
    }
    let namespace = tool_name.namespace.as_deref()?;
    let rest = namespace.strip_prefix(root)?;
    let plugin = if rest.is_empty() || rest == "__" {
        None
    } else {
        let plugin = rest.strip_prefix("__")?.trim_end_matches('_');
        Some(codex_code_mode::normalize_code_mode_identifier(plugin))
    };
    Some(HostTool {
        name: codex_code_mode::normalize_code_mode_identifier(&tool_name.name),
        plugin,
    })
}

#[cfg(test)]
#[path = "code_mode_names_tests.rs"]
mod tests;
