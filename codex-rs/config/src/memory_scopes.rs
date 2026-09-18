//! Fork addition: naming helpers for per-chat ("scoped") Codex memories.
//!
//! A thread configured with `memories.scope_key` keeps its private memories in
//! `<codex_home>/<memories dir>_scopes/<scope dir>/`, a sibling of the global
//! memory root, and its extracted memories live in the `scope:<key>` partition
//! of the memory database. Threads without a scope key use only the global
//! store, exactly as upstream.

use codex_utils_absolute_path::AbsolutePathBuf;

/// Database partition (and Phase 2 job key) of the global memory store.
pub const GLOBAL_MEMORY_PARTITION: &str = "global";

/// Database partition (and Phase 2 job key) of one memory scope.
pub fn memory_scope_partition(scope_key: &str) -> String {
    format!("scope:{scope_key}")
}

/// Filesystem-safe, collision-resistant directory name for a scope key.
///
/// Keys made only of lowercase ASCII letters, digits, `-` and `_` (at most 64
/// chars) are used verbatim; every other key gets a sanitized prefix plus a
/// `-<16 hex>` hash suffix. Verbatim names never end in such a suffix and are
/// lowercase, so two distinct keys cannot share a directory, including on
/// case-insensitive filesystems (Windows, macOS). Windows device names are
/// hashed too.
pub fn memory_scope_dir_name(scope_key: &str) -> String {
    let sanitized = scope_key
        .chars()
        .map(|ch| {
            if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_' {
                ch
            } else if ch.is_ascii_uppercase() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(64)
        .collect::<String>();
    if sanitized == scope_key && !has_hash_suffix(&sanitized) && !is_windows_device_name(&sanitized)
    {
        return sanitized;
    }
    // FNV-1a keeps the suffix stable across builds and platforms.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in scope_key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{sanitized}-{hash:016x}")
}

/// Whether `name` ends like a hashed directory name (`-<16 lowercase hex>`).
fn has_hash_suffix(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 17
        && bytes[bytes.len() - 17] == b'-'
        && bytes[bytes.len() - 16..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn is_windows_device_name(name: &str) -> bool {
    matches!(name, "con" | "prn" | "aux" | "nul")
        || ((name.starts_with("com") || name.starts_with("lpt"))
            && name.len() == 4
            && name.as_bytes()[3].is_ascii_digit())
}

/// Root directory of one memory scope for a memories directory name
/// (`MemoryVersion::directory_name()`, e.g. `memories` or `memories_v2`).
pub fn memory_scope_root(
    codex_home: &AbsolutePathBuf,
    memories_dir_name: &str,
    scope_key: &str,
) -> AbsolutePathBuf {
    codex_home
        .join(format!("{memories_dir_name}_scopes"))
        .join(memory_scope_dir_name(scope_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn scope_dir_names_are_safe_and_distinct() {
        assert_eq!(memory_scope_dir_name("group-42_a"), "group-42_a");
        let odd = memory_scope_dir_name("../qq:123/..");
        assert!(odd.starts_with("___qq_123___-"));
        assert!(!odd.contains('/') && !odd.contains('.'));
        assert_ne!(memory_scope_dir_name("a:b"), memory_scope_dir_name("a/b"));
        assert_ne!(memory_scope_dir_name("a_b"), memory_scope_dir_name("a:b"));
        // A verbatim key cannot impersonate the hashed directory of another key.
        let hashed = memory_scope_dir_name("a:b");
        assert_ne!(memory_scope_dir_name(&hashed), hashed);
        // Case-only differences stay distinct on case-insensitive filesystems.
        assert_ne!(
            memory_scope_dir_name("Chat").to_ascii_lowercase(),
            memory_scope_dir_name("chat").to_ascii_lowercase()
        );
        assert_ne!(memory_scope_dir_name("con"), "con");
        assert_ne!(memory_scope_dir_name("com1"), "com1");
        assert_eq!(memory_scope_partition("chat"), "scope:chat");
    }
}
