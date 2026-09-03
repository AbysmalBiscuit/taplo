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
