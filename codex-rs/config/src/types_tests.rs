use super::*;
use pretty_assertions::assert_eq;

#[test]
fn deserialize_skill_config_with_name_selector() {
    let cfg: SkillConfig = toml::from_str(
        r#"
            name = "github:yeet"
            enabled = false
        "#,
    )
    .expect("should deserialize skill config with name selector");

    assert_eq!(cfg.name.as_deref(), Some("github:yeet"));
    assert_eq!(cfg.path, None);
    assert!(!cfg.enabled);
}

#[test]
fn deserialize_skill_config_with_path_selector() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let skill_path = tempdir.path().join("skills").join("demo").join("SKILL.md");
    let cfg: SkillConfig = toml::from_str(&format!(
        r#"
            path = {path:?}
            enabled = false
        "#,
        path = skill_path.display().to_string(),
    ))
    .expect("should deserialize skill config with path selector");

    assert_eq!(
        cfg,
        SkillConfig {
            path: Some(
                AbsolutePathBuf::from_absolute_path(&skill_path)
                    .expect("skill path should be absolute"),
            ),
            name: None,
            enabled: false,
        }
    );
}

#[test]
fn memories_config_clamps_count_limits_to_nonzero_values() {
    let config = MemoriesConfig::from(MemoriesToml {
        max_raw_memories_for_consolidation: Some(0),
        max_rollouts_per_startup: Some(0),
        ..Default::default()
    });

    assert_eq!(
        config,
        MemoriesConfig {
            max_raw_memories_for_consolidation: 1,
            max_rollouts_per_startup: 1,
            ..MemoriesConfig::default()
        }
    );
}

#[test]
fn memories_config_clamps_rate_limit_remaining_threshold() {
    let config = MemoriesConfig::from(MemoriesToml {
        min_rate_limit_remaining_percent: Some(101),
        ..Default::default()
    });
    assert_eq!(
        config,
        MemoriesConfig {
            min_rate_limit_remaining_percent: 100,
            ..MemoriesConfig::default()
        }
    );

    let config = MemoriesConfig::from(MemoriesToml {
        min_rate_limit_remaining_percent: Some(-1),
        ..Default::default()
    });
    assert_eq!(
        config,
        MemoriesConfig {
            min_rate_limit_remaining_percent: 0,
            ..MemoriesConfig::default()
        }
    );
}

// Fork addition: entry-level memory deletion switch.
#[test]
fn memories_may_delete_defaults_to_false_and_is_omitted_when_unset() {
    let parsed: MemoriesToml = toml::from_str("").expect("parse memories config");
    assert_eq!(parsed.may_delete, None);
    assert!(!MemoriesConfig::from(parsed).may_delete);
    assert!(!MemoriesConfig::default().may_delete);

    let serialized =
        serde_json::to_value(MemoriesConfig::default()).expect("serialize memories config");
    assert!(
        serialized.get("may_delete").is_none(),
        "the default must serialize exactly like upstream"
    );

    let parsed: MemoriesToml = toml::from_str("may_delete = true").expect("parse memories config");
    let config = MemoriesConfig::from(parsed);
    assert_eq!(
        config,
        MemoriesConfig {
            may_delete: true,
            ..MemoriesConfig::default()
        }
    );
    assert_eq!(
        serde_json::to_value(config).expect("serialize memories config")["may_delete"],
        serde_json::json!(true)
    );
}

#[test]
fn memories_version_selects_pipeline_without_changing_other_defaults() {
    for (source, version) in [
        ("", MemoryVersion::V1),
        ("version = \"v2\"", MemoryVersion::V2),
    ] {
        let parsed: MemoriesToml = toml::from_str(source).expect("parse memories config");
        assert_eq!(
            MemoriesConfig::from(parsed),
            MemoriesConfig {
                version,
                ..Default::default()
            }
        );
    }
    assert!(toml::from_str::<MemoriesToml>("version = \"v3\"").is_err());
}
