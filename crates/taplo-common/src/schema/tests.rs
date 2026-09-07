use super::*;
use crate::environment::native::NativeEnvironment;
use serde_json::json;

const TEST_SCHEMA_URL: &str = "file:///taplo-test/schema.json";

/// Seeds the in-memory schema cache so that lookups never reach the network.
async fn seeded(schema: Value) -> (Schemas<NativeEnvironment>, Url) {
    let schemas = Schemas::new(NativeEnvironment::new(), reqwest::Client::new());
    let url = Url::parse(TEST_SCHEMA_URL).unwrap();
    drop(schemas.cache().store(url.clone(), Arc::new(schema)).await);
    (schemas, url)
}

fn schema_at<'s>(children: &'s [(Keys, Keys, Arc<Value>)], path: &str) -> Option<&'s Arc<Value>> {
    children
        .iter()
        .find(|(_, p, _)| p.to_string() == path)
        .map(|(_, _, s)| s)
}

#[tokio::test]
async fn composed_all_of_ref_resolves_to_child_schema() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "value": {
                "description": "Executable names checked on PATH.",
                "allOf": [{ "$ref": "#/definitions/value" }]
            }
        },
        "definitions": {
            "value": { "type": "string" }
        }
    }))
    .await;

    let children = schemas
        .possible_schemas_from(&url, &Value::Null, &Keys::empty(), 5)
        .await
        .unwrap();

    let value = schema_at(&children, "value").expect("no schema collected for `value`");
    assert_eq!(value["type"], "string");
    assert_eq!(value["description"], "Executable names checked on PATH.");
}

#[tokio::test]
async fn self_referential_all_of_terminates() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "node": { "$ref": "#/definitions/node" }
        },
        "definitions": {
            "node": {
                "description": "Points back at itself.",
                "allOf": [{ "$ref": "#/definitions/node" }]
            }
        }
    }))
    .await;

    schemas
        .possible_schemas_from(&url, &Value::Null, &Keys::empty(), 5)
        .await
        .unwrap();
}

#[tokio::test]
async fn mutually_referential_all_of_terminates() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "a": { "$ref": "#/definitions/a" }
        },
        "definitions": {
            "a": { "allOf": [{ "$ref": "#/definitions/b" }] },
            "b": { "allOf": [{ "$ref": "#/definitions/a" }] }
        }
    }))
    .await;

    schemas
        .possible_schemas_from(&url, &Value::Null, &Keys::empty(), 5)
        .await
        .unwrap();
}

/// Collects `description` from each schema so assertions name which branch
/// of the schema was reached rather than comparing whole JSON values.
fn descriptions(schemas: &[(Keys, Arc<Value>)]) -> Vec<&str> {
    schemas
        .iter()
        .filter_map(|(_, s)| s["description"].as_str())
        .collect()
}

#[tokio::test]
async fn prefix_items_resolves_at_covered_index() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "pair": {
                "type": "array",
                "prefixItems": [
                    { "type": "string", "description": "head" },
                    { "type": "integer", "description": "second" }
                ]
            }
        }
    }))
    .await;

    let keys = "pair".parse::<Keys>().unwrap().join(0_usize);
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["head"]);
}

#[tokio::test]
async fn prefix_items_falls_through_to_items_past_the_end() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "pair": {
                "type": "array",
                "prefixItems": [{ "type": "string", "description": "head" }],
                "items": { "type": "integer", "description": "tail" }
            }
        }
    }))
    .await;

    let keys = "pair".parse::<Keys>().unwrap().join(1_usize);
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["tail"]);
}

#[tokio::test]
async fn items_does_not_apply_at_an_index_prefix_items_covers() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "pair": {
                "type": "array",
                "prefixItems": [{ "type": "string", "description": "head" }],
                "items": { "type": "integer", "description": "tail" }
            }
        }
    }))
    .await;

    let keys = "pair".parse::<Keys>().unwrap().join(0_usize);
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["head"]);
}

#[tokio::test]
async fn draft_7_tuple_items_still_resolve_by_position() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "pair": {
                "type": "array",
                "items": [
                    { "type": "string", "description": "head" },
                    { "type": "integer", "description": "second" }
                ]
            }
        }
    }))
    .await;

    let keys = "pair".parse::<Keys>().unwrap().join(1_usize);
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["second"]);
}

#[tokio::test]
async fn single_schema_items_still_resolve_at_every_index() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "list": {
                "type": "array",
                "items": { "type": "string", "description": "every element" }
            }
        }
    }))
    .await;

    let keys = "list".parse::<Keys>().unwrap().join(3_usize);
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["every element"]);
}

/// Reads the draft the cached validator was compiled against, which is the
/// only direct evidence of which draft's rules a schema ran under.
fn compiled_draft(schemas: &Schemas<NativeEnvironment>, url: &Url) -> Draft {
    schemas
        .validators
        .lock()
        .get(url)
        .cloned()
        .expect("no validator was cached")
        .draft()
}

/// `{"a": "x", "b": 1}` leaves `b` unevaluated, which only 2019-09 and
/// 2020-12 report.
fn unevaluated_properties_schema(declared: &str) -> Value {
    json!({
        "$schema": declared,
        "type": "object",
        "properties": { "a": { "type": "string" } },
        "unevaluatedProperties": false
    })
}

async fn errors_for(schema: Value) -> (Vec<String>, Draft) {
    let (schemas, url) = seeded(schema).await;
    let instance = json!({ "a": "x", "b": 1 });
    let errors = schemas.validate(&url, &instance).await.unwrap();
    let draft = compiled_draft(&schemas, &url);
    (errors.iter().map(ToString::to_string).collect(), draft)
}

#[tokio::test]
async fn draft_2020_12_without_a_fragment_is_honored() {
    let (errors, draft) = errors_for(unevaluated_properties_schema(
        "https://json-schema.org/draft/2020-12/schema",
    ))
    .await;

    assert_eq!(draft, Draft::Draft202012);
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[tokio::test]
async fn draft_2020_12_with_a_fragment_is_honored() {
    let (errors, draft) = errors_for(unevaluated_properties_schema(
        "https://json-schema.org/draft/2020-12/schema#",
    ))
    .await;

    assert_eq!(draft, Draft::Draft202012);
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[tokio::test]
async fn draft_2019_09_is_honored() {
    let (errors, draft) = errors_for(unevaluated_properties_schema(
        "https://json-schema.org/draft/2019-09/schema",
    ))
    .await;

    assert_eq!(draft, Draft::Draft201909);
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[tokio::test]
async fn an_unsupported_draft_falls_back_to_draft_7() {
    let (_, draft) = errors_for(unevaluated_properties_schema(
        "http://json-schema.org/draft-03/schema#",
    ))
    .await;

    assert_eq!(draft, Draft::Draft7);
}

#[tokio::test]
async fn a_schema_without_a_declaration_is_draft_7() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": { "a": { "type": "string" } }
    }))
    .await;

    schemas.validate(&url, &json!({ "a": "x" })).await.unwrap();

    assert_eq!(compiled_draft(&schemas, &url), Draft::Draft7);
}

#[tokio::test]
async fn custom_formats_still_assert_under_draft_2020_12() {
    let (schemas, url) = seeded(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": { "version": { "type": "string", "format": "semver" } }
    }))
    .await;

    let errors = schemas
        .validate(&url, &json!({ "version": "not a version" }))
        .await
        .unwrap();

    assert_eq!(compiled_draft(&schemas, &url), Draft::Draft202012);
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn declared_draft_classifies_every_meta_schema_uri() {
    let classify = |declared: &str| declared_draft(&json!({ "$schema": declared }));

    assert_eq!(
        classify("http://json-schema.org/draft-07/schema#"),
        DeclaredDraft::Supported(Draft::Draft7)
    );
    assert_eq!(
        classify("https://json-schema.org/draft-07/schema"),
        DeclaredDraft::Supported(Draft::Draft7)
    );
    assert_eq!(
        classify("http://json-schema.org/draft-04/schema#"),
        DeclaredDraft::Supported(Draft::Draft4)
    );
    assert_eq!(
        classify("http://json-schema.org/draft-06/schema#"),
        DeclaredDraft::Supported(Draft::Draft6)
    );
    assert_eq!(
        classify("https://json-schema.org/draft/2019-09/schema"),
        DeclaredDraft::Supported(Draft::Draft201909)
    );
    assert_eq!(
        classify("https://json-schema.org/draft/2020-12/schema"),
        DeclaredDraft::Supported(Draft::Draft202012)
    );

    assert_eq!(
        classify("http://json-schema.org/draft-03/schema#"),
        DeclaredDraft::Unsupported("http://json-schema.org/draft-03/schema".to_owned())
    );
    assert_eq!(
        classify("https://json-schema.org/draft/next/schema"),
        DeclaredDraft::Unsupported("https://json-schema.org/draft/next/schema".to_owned())
    );

    assert_eq!(
        classify("https://example.com/my-meta-schema.json"),
        DeclaredDraft::Unrecognized
    );
    assert_eq!(classify("not a url"), DeclaredDraft::Unrecognized);
    assert_eq!(declared_draft(&json!({})), DeclaredDraft::Unrecognized);
    assert_eq!(
        declared_draft(&Value::Bool(true)),
        DeclaredDraft::Unrecognized
    );
}
