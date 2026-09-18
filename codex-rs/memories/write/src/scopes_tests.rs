use super::*;
use pretty_assertions::assert_eq;

fn scoped(classify: bool) -> ThreadRouting {
    ThreadRouting::Scoped {
        scope_key: "chat-1".to_string(),
        classify,
    }
}

#[test]
fn routing_follows_registered_scope() {
    assert_eq!(ThreadRouting::from_scope(None), ThreadRouting::Upstream);
    assert_eq!(
        ThreadRouting::from_scope(Some(ThreadMemoryScope {
            scope_key: None,
            may_write_global: false,
        })),
        ThreadRouting::Drop
    );
    assert_eq!(
        ThreadRouting::from_scope(Some(ThreadMemoryScope {
            scope_key: Some("chat-1".to_string()),
            may_write_global: true,
        })),
        scoped(/*classify*/ true)
    );
    assert_eq!(
        ThreadRouting::from_scope(Some(ThreadMemoryScope {
            scope_key: Some("chat-1".to_string()),
            may_write_global: false,
        })),
        scoped(/*classify*/ false)
    );
}

#[test]
fn partition_is_global_only_for_explicit_global_verdict_of_global_writer() {
    assert_eq!(ThreadRouting::Upstream.partition(None), None);
    assert_eq!(
        scoped(true).partition(Some(Visibility::Global)),
        Some("global".to_string())
    );
    assert_eq!(
        scoped(true).partition(Some(Visibility::Private)),
        Some("scope:chat-1".to_string())
    );
    assert_eq!(
        scoped(true).partition(None),
        Some("scope:chat-1".to_string())
    );
    // A thread that may not write globally never reaches the global store.
    assert_eq!(
        scoped(false).partition(Some(Visibility::Global)),
        Some("scope:chat-1".to_string())
    );
}

#[test]
fn split_visibility_defaults_to_private_and_strips_the_field() {
    let (rest, visibility) = split_visibility(
        r#"{"rollout_summary":"s","rollout_slug":null,"raw_memory":"r","memory_visibility":"global"}"#,
    );
    assert_eq!(visibility, Visibility::Global);
    let parsed: Value = serde_json::from_str(&rest).expect("json");
    assert_eq!(parsed.get(VISIBILITY_FIELD), None);
    assert!(
        crate::phase1_output::StageOneOutput::parse(&rest, MemoryVersion::V1).is_ok(),
        "stripped output must satisfy the upstream parser"
    );

    for source in [
        r#"{"rollout_summary":"s","rollout_slug":null,"raw_memory":"r"}"#,
        r#"{"rollout_summary":"s","rollout_slug":null,"raw_memory":"r","memory_visibility":"public"}"#,
        r#"{"rollout_summary":"s","rollout_slug":null,"raw_memory":"r","memory_visibility":true}"#,
        "not json",
    ] {
        assert_eq!(split_visibility(source).1, Visibility::Private, "{source}");
    }
}

#[test]
fn classification_extends_schema_and_instructions() {
    for version in [MemoryVersion::V1, MemoryVersion::V2] {
        let mut schema = crate::phase1_output::output_schema(version);
        let mut instructions = String::from("base");
        add_classification(&mut instructions, &mut schema);
        assert!(instructions.starts_with("base"));
        assert!(instructions.contains("When in doubt, choose \"private\""));
        assert_eq!(
            schema["properties"][VISIBILITY_FIELD],
            serde_json::json!({ "type": "string", "enum": ["global", "private"] })
        );
        assert!(
            schema["required"]
                .as_array()
                .expect("required")
                .contains(&Value::String(VISIBILITY_FIELD.to_string()))
        );
    }
}

#[test]
fn fork_mode_is_off_by_default() {
    assert!(!fork_mode_active(&MemoriesConfig::default()));
    let scoped = MemoriesConfig {
        scope_key: Some("chat".to_string()),
        ..MemoriesConfig::default()
    };
    assert!(fork_mode_active(&scoped));
    let manual = MemoriesConfig {
        auto_consolidate: false,
        ..MemoriesConfig::default()
    };
    assert!(fork_mode_active(&manual));
}
