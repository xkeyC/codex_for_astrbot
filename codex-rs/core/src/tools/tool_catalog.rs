//! Fork addition: the deferred nested tools a code-mode `exec` script can
//! call, for the `tool_catalog` world-state section.
//!
//! Deferred tools stay out of the `exec` description so adding or removing
//! them does not change the prompt prefix; without a listing the model only
//! finds them by searching `ALL_TOOLS`. The catalog names them in history
//! instead, where changes append rather than rewrite.

use std::collections::HashSet;

use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolExposure;
use codex_tools::ToolName;
use codex_tools::ToolSpec;

use crate::tools::registry::ToolRegistry;

/// One deferred tool as the code-mode runtime names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogTool {
    /// Name on the global `tools` object, e.g. `astrbot__web_search`.
    pub(crate) global_name: String,
    /// The `namespace__` part of `global_name`; empty for plain tools.
    pub(crate) group: String,
    pub(crate) description: String,
    pub(crate) group_description: String,
}

/// Deferred tools nested code mode exposes, in registry order, skipping the
/// same ones `register_code_mode_executors` skips.
pub(crate) fn deferred_code_mode_tools(
    registry: &ToolRegistry,
    excluded_namespaces: &[String],
) -> Vec<CatalogTool> {
    let mut taken = HashSet::new();
    let mut tools = Vec::new();
    for tool in registry.entries() {
        let tool_name = tool.runtime.tool_name();
        if tool.exposure != ToolExposure::Deferred
            || tool_name
                .clone()
                .with_default_namespace()
                .namespace
                .as_ref()
                .is_some_and(|namespace| excluded_namespaces.contains(namespace))
        {
            continue;
        }
        let owned_spec;
        let spec = if let Some(spec) = tool.runtime.immutable_spec() {
            spec.as_ref()
        } else {
            owned_spec = tool.runtime.spec();
            &owned_spec
        };
        let (description, group_description) = match spec {
            ToolSpec::Function(function) => (function.description.as_str(), ""),
            ToolSpec::Freeform(freeform) => (freeform.description.as_str(), ""),
            ToolSpec::Namespace(namespace) => {
                let Some(description) = namespace.tools.iter().find_map(|nested| match nested {
                    ResponsesApiNamespaceTool::Function(function)
                        if function.name == tool_name.name =>
                    {
                        Some(function.description.as_str())
                    }
                    ResponsesApiNamespaceTool::Custom(custom) if custom.name == tool_name.name => {
                        Some(custom.description.as_str())
                    }
                    _ => None,
                }) else {
                    continue;
                };
                (description, namespace.description.as_str())
            }
            ToolSpec::ToolSearch { .. } | ToolSpec::WebSearch { .. } => continue,
        };
        let code_mode_name = codex_tools::code_mode_name_for_tool_name(&tool_name);
        if !codex_code_mode::is_code_mode_nested_tool(&code_mode_name) {
            continue;
        }
        let global_name = codex_code_mode::normalize_code_mode_identifier(&code_mode_name);
        // The code-mode runtime keeps the first tool of a normalized name.
        if !taken.insert(global_name.clone()) {
            continue;
        }
        tools.push(CatalogTool {
            group: group_of(&tool_name, &global_name),
            global_name,
            description: description.to_string(),
            group_description: group_description.to_string(),
        });
    }
    tools
}

fn group_of(tool_name: &ToolName, global_name: &str) -> String {
    let Some(namespace) = tool_name.namespace.as_deref() else {
        return String::new();
    };
    if tool_name.is_default_namespace() {
        return String::new();
    }
    let prefix = codex_code_mode::normalize_code_mode_identifier(
        &codex_tools::code_mode_name_for_tool_name(&ToolName::namespaced(namespace, "")),
    );
    if global_name.len() > prefix.len() && global_name.starts_with(&prefix) {
        prefix
    } else {
        String::new()
    }
}

#[cfg(test)]
#[path = "tool_catalog_tests.rs"]
mod tests;
