use pretty_assertions::assert_eq;
use serde_json::json;

use super::check_arguments;

fn search_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" },
            "limit": { "type": "number" },
            "page": { "type": "integer" },
            "recency": { "type": "string", "enum": ["day", "week"] },
            "tags": { "type": "array", "items": { "type": "string" } },
            "filter": {
                "type": "object",
                "properties": { "site": { "type": ["string", "null"] } },
                "additionalProperties": false
            }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

#[test]
fn reports_the_first_violation_with_its_path() {
    let schema = search_schema();
    for (args, expected) in [
        (
            json!({ "query": "x", "limit": "5" }),
            r#"args.limit must be number, got string "5""#,
        ),
        (json!({ "limit": 5 }), "args.query is required"),
        (
            json!({ "query": "x", "limt": 5 }),
            "args.limt is not an allowed property (allowed: filter, limit, page, query, recency, tags)",
        ),
        (
            json!({ "query": "x", "recency": "month" }),
            r#"args.recency must be one of "day", "week", got "month""#,
        ),
        (
            json!({ "query": "x", "page": 1.5 }),
            "args.page must be integer, got number 1.5",
        ),
        (
            json!({ "query": "x", "tags": ["a", 2] }),
            "args.tags[1] must be string, got number 2",
        ),
        (
            json!({ "query": "x", "filter": { "site": 3 } }),
            "args.filter.site must be string | null, got number 3",
        ),
        (
            json!({ "query": "x", "filter": { "my-site": "a" } }),
            r#"args.filter["my-site"] is not an allowed property (allowed: site)"#,
        ),
        (json!("x"), r#"args must be object, got string "x""#),
    ] {
        assert_eq!(
            check_arguments(&schema, &args).as_deref(),
            Some(expected),
            "{args}"
        );
    }
}

#[test]
fn accepts_what_the_schema_allows() {
    let schema = search_schema();
    for args in [
        json!({ "query": "x" }),
        json!({ "query": "x", "limit": 5, "page": 2.0, "recency": "day" }),
        json!({ "query": "x", "tags": [], "filter": { "site": null } }),
    ] {
        assert_eq!(check_arguments(&schema, &args), None, "{args}");
    }
}

#[test]
fn any_of_and_one_of_accept_when_any_branch_matches() {
    let schema = json!({
        "type": "object",
        "properties": {
            "id": { "anyOf": [{ "type": "string" }, { "type": "integer" }] },
            "mode": { "oneOf": [{ "const": "fast" }, { "const": "slow" }, { "type": "number" }] }
        }
    });
    for args in [
        json!({ "id": "a" }),
        json!({ "id": 3 }),
        json!({ "mode": "slow" }),
        json!({ "mode": 2 }),
    ] {
        assert_eq!(check_arguments(&schema, &args), None, "{args}");
    }
    assert_eq!(
        check_arguments(&schema, &json!({ "id": true })).as_deref(),
        Some(
            "args.id matches none of the 2 allowed shapes (args.id must be string, got boolean true)"
        )
    );
    assert_eq!(
        check_arguments(&schema, &json!({ "mode": "medium" })).as_deref(),
        Some(
            r#"args.mode matches none of the 3 allowed shapes (args.mode must be "fast", got "medium")"#
        )
    );
}

#[test]
fn never_rejects_what_it_does_not_understand() {
    for (schema, args) in [
        // Unresolved references, unknown types and keywords.
        (
            json!({ "type": "object", "properties": { "a": { "$ref": "#/$defs/a" } }, "additionalProperties": false }),
            json!({ "a": 1 }),
        ),
        (json!({ "type": "custom" }), json!(1)),
        (
            json!({ "type": "string", "format": "email", "minLength": 99 }),
            json!("x"),
        ),
        // Properties other keywords may allow.
        (
            json!({ "type": "object", "patternProperties": { "^x": {} }, "additionalProperties": false }),
            json!({ "xa": 1 }),
        ),
        (
            json!({ "allOf": [{ "properties": { "b": {} } }], "properties": { "a": {} }, "additionalProperties": false }),
            json!({ "a": 1, "b": 2 }),
        ),
        // Additional properties are allowed unless the schema says otherwise.
        (
            json!({ "type": "object", "properties": { "a": {} } }),
            json!({ "z": 1 }),
        ),
        // OpenAPI `nullable`, tuple `items`, numbers compared by value.
        (json!({ "type": "string", "nullable": true }), json!(null)),
        (
            json!({ "type": "array", "items": [{ "type": "string" }] }),
            json!([1]),
        ),
        (json!({ "enum": [1, 2] }), json!(2.0)),
        (json!({ "const": { "a": [1] } }), json!({ "a": [1.0] })),
        (json!(true), json!({ "anything": 1 })),
        (json!({}), json!([1, "a"])),
    ] {
        assert_eq!(check_arguments(&schema, &args), None, "{schema} {args}");
    }
}

#[test]
fn additional_properties_schemas_apply_to_extra_keys() {
    let schema = json!({ "type": "object", "additionalProperties": { "type": "number" } });
    assert_eq!(check_arguments(&schema, &json!({ "a": 1 })), None);
    assert_eq!(
        check_arguments(&schema, &json!({ "a": "1" })).as_deref(),
        Some(r#"args.a must be number, got string "1""#)
    );
    assert_eq!(
        check_arguments(&json!(false), &json!(1)).as_deref(),
        Some("args is not allowed")
    );
}
