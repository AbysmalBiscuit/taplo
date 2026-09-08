use super::*;
use crate::environment::native::NativeEnvironment;
use serde_json::json;

const TEST_SCHEMA_URL: &str = "file:///taplo-test/schema.json";

#[tokio::test]
async fn embedded_resource_children_keep_the_resource_scope() {
    let (schemas, url) = seeded(json!({
        "properties": {"server": {"$ref": "defs/network.json#/$defs/server"}},
        "$defs": {
            "network": {"$id": "defs/network.json", "$defs": {
                "server": {"properties": {"port": {"$ref": "port.json"}}}
            }},
            "port": {"$id": "defs/port.json", "type": "integer", "description": "Port"}
        }
    }))
    .await;
    let found = schemas
        .possible_schemas_from(&url, &json!({"server": {}}), &"server".parse().unwrap(), 2)
        .await
        .unwrap();
    assert_eq!(schema_at(&found, "port").unwrap()["description"], "Port");
}

#[tokio::test]
async fn external_references_load_before_validation() {
    let schemas = Schemas::new(NativeEnvironment::new(), reqwest::Client::new());
    let url = Url::from_file_path(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema/root.json"),
    )
    .unwrap();
    let invalid = taplo::parser::parse("port = 70000\n").into_dom();
    let errors = schemas.validate_root(&url, &invalid).await.unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].keys.to_string(), "port");
    assert!(!errors[0].text_ranges().collect::<Vec<_>>().is_empty());
    let valid = taplo::parser::parse("port = 443\n").into_dom();
    assert!(schemas
        .validate_root(&url, &valid)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn unevaluated_items_skips_positions_covered_by_contains() {
    let (schemas, url) = seeded(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "properties": { "values": {
            "contains": {"type": "integer"},
            "unevaluatedItems": {"type": "string", "description": "Tail"}
        }}
    }))
    .await;
    let value = json!({"values": [1, "ok"]});
    let path = "values".parse::<Keys>().unwrap();
    let head = schemas
        .schemas_at_path(&url, &value, &path.join(0_usize))
        .await
        .unwrap();
    assert!(head.is_empty());
    let tail = schemas
        .schemas_at_path(&url, &value, &path.join(1_usize))
        .await
        .unwrap();
    assert_eq!(descriptions(&tail), ["Tail"]);
    assert!(schemas.validate(&url, &value).await.unwrap().is_empty());
}

#[tokio::test]
async fn unevaluated_items_ignores_failed_any_of_branches() {
    let (schemas, url) = seeded(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "properties": { "values": {
            "anyOf": [
                {"prefixItems": [{"const": 1}]},
                {"prefixItems": [{"const": 2}, {"type": "string"}]}
            ],
            "unevaluatedItems": {"type": "string", "description": "Tail"}
        }}
    }))
    .await;
    let found = schemas
        .schemas_at_path(
            &url,
            &json!({"values": [1, "ok"]}),
            &"values".parse::<Keys>().unwrap().join(1_usize),
        )
        .await
        .unwrap();
    assert_eq!(descriptions(&found), ["Tail"]);
}

#[tokio::test]
async fn remaining_all_of_preserves_annotations_and_intersects_enums() {
    let (schemas, url) = seeded(json!({
        "properties": { "mode": {
            "description": "Choose a mode",
            "enum": ["fast", "safe"],
            "allOf": [{ "type": "string", "enum": ["safe", "other"] }]
        }}
    }))
    .await;
    let path = "mode".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({"mode": "safe"}), &path)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1["description"], "Choose a mode");
    assert_eq!(found[0].1["enum"], json!(["safe"]));
    let children = schemas
        .possible_schemas_from(&url, &Value::Null, &Keys::empty(), 5)
        .await
        .unwrap();
    assert_eq!(
        schema_at(&children, "mode").unwrap()["enum"],
        json!(["safe"])
    );
}

#[tokio::test]
async fn remaining_embedded_resource_resolves_without_fetching_a_file() {
    let (schemas, url) = seeded(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "properties": { "port": { "$ref": "defs/network.json#port" } },
        "$defs": { "network": {
            "$id": "defs/network.json",
            "$defs": { "port": { "$anchor": "port", "description": "Network port", "type": "integer" } }
        }}
    })).await;
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();
    assert_eq!(descriptions(&found), ["Network port"]);
    assert!(!schemas
        .validate(&url, &json!({"port": "bad"}))
        .await
        .unwrap()
        .is_empty());
    assert!(schemas
        .validate(&url, &json!({"port": 80}))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn remaining_unevaluated_items_validate_and_supply_tail_schemas() {
    let (schemas, url) = seeded(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "properties": { "values": {
            "type": "array",
            "allOf": [{ "prefixItems": [{ "type": "integer", "description": "Head" }] }],
            "unevaluatedItems": { "type": "string", "description": "Tail", "enum": ["ok"] }
        }}
    }))
    .await;
    let dom = taplo::parser::parse("values = [1, 2]\n").into_dom();
    assert!(!schemas.validate_root(&url, &dom).await.unwrap().is_empty());
    assert!(schemas
        .validate(&url, &json!({"values": [1, "ok"]}))
        .await
        .unwrap()
        .is_empty());
    for (index, expected) in [(0_usize, "Head"), (1, "Tail")] {
        let found = schemas
            .schemas_at_path(
                &url,
                &json!({"values": [1, "ok"]}),
                &"values".parse::<Keys>().unwrap().join(index),
            )
            .await
            .unwrap();
        assert_eq!(descriptions(&found), [expected]);
    }
}

#[tokio::test]
async fn dynamic_and_recursive_refs_validate_nested_toml() {
    for (draft, anchor, reference) in [
        (
            "2020-12",
            json!({"$dynamicAnchor": "node"}),
            json!({"$dynamicRef": "#node"}),
        ),
        (
            "2019-09",
            json!({"$recursiveAnchor": true}),
            json!({"$recursiveRef": "#"}),
        ),
    ] {
        let mut schema = json!({
            "$schema": format!("https://json-schema.org/draft/{draft}/schema"),
            "type": "object",
            "properties": { "value": { "type": "integer" }, "child": reference }
        });
        schema
            .as_object_mut()
            .unwrap()
            .extend(anchor.as_object().unwrap().clone());
        let (schemas, url) = seeded(schema).await;
        let invalid = taplo::parser::parse("value = 1\n[child]\nvalue = \"bad\"\n").into_dom();
        let errors = schemas.validate_root(&url, &invalid).await.unwrap();
        assert_eq!(errors.len(), 1, "{draft}: {errors:?}");
        assert_eq!(errors[0].keys.to_string(), "child.value");
        let valid = taplo::parser::parse("value = 1\n[child]\nvalue = 2\n").into_dom();
        assert!(schemas
            .validate_root(&url, &valid)
            .await
            .unwrap()
            .is_empty());
    }
}

#[tokio::test]
async fn all_of_required_keys_are_a_union() {
    let (schemas, url) = seeded(json!({
        "properties": { "config": {
            "required": ["a"],
            "allOf": [{"required": ["a", "b"]}, {"required": ["c"]}]
        }}
    }))
    .await;
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"config".parse().unwrap())
        .await
        .unwrap();
    assert_eq!(found[0].1["required"], json!(["a", "b", "c"]));
}

#[tokio::test]
async fn embedded_resource_pointer_keeps_relative_refs_scoped() {
    let (schemas, url) = seeded(json!({
        "$id": "https://example.com/root.json",
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "properties": {"port": {"$ref": "defs/network.json#/$defs/port"}},
        "$defs": {
            "network": {"$id": "defs/network.json", "$defs": {"port": {"$ref": "port.json"}}},
            "port": {"$id": "defs/port.json", "type": "integer", "description": "Port"}
        }
    }))
    .await;
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse().unwrap())
        .await
        .unwrap();
    assert_eq!(descriptions(&found), ["Port"]);
    assert!(!schemas
        .validate(&url, &json!({"port": "bad"}))
        .await
        .unwrap()
        .is_empty());
}

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

#[tokio::test]
async fn self_referential_all_of_terminates_at_a_non_empty_path() {
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

    let keys = "node".parse::<Keys>().unwrap();

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["Points back at itself."]);
}

/// A schema whose `image` key is described differently by each branch of one
/// condition, so that the description names the branch that was taken.
fn conditional_schema() -> Value {
    json!({
        "type": "object",
        "if": {
            "properties": { "kind": { "const": "docker" } },
            "required": ["kind"]
        },
        "then": { "properties": { "image": { "type": "string", "description": "then" } } },
        "else": { "properties": { "image": { "type": "string", "description": "else" } } }
    })
}

#[tokio::test]
async fn a_met_condition_selects_the_then_branch() {
    let (schemas, url) = seeded(conditional_schema()).await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "docker" }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["then"]);
}

#[tokio::test]
async fn an_unmet_condition_selects_the_else_branch() {
    let (schemas, url) = seeded(conditional_schema()).await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "podman" }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["else"]);
}

#[tokio::test]
async fn an_absent_instance_takes_both_branches() {
    let (schemas, url) = seeded(conditional_schema()).await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["then", "else"]);
}

#[tokio::test]
async fn an_empty_instance_decides_the_condition() {
    let (schemas, url) = seeded(conditional_schema()).await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({}), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["else"]);
}

#[tokio::test]
async fn a_condition_that_is_a_reference_is_resolved() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": { "$ref": "#/definitions/docker" },
        "then": { "properties": { "image": { "description": "then" } } },
        "else": { "properties": { "image": { "description": "else" } } },
        "definitions": {
            "docker": {
                "properties": { "kind": { "const": "docker" } },
                "required": ["kind"]
            }
        }
    }))
    .await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "docker" }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["then"]);
}

#[tokio::test]
async fn a_condition_holding_a_nested_reference_decides_its_branch() {
    let schema = json!({
        "type": "object",
        "if": {
            "properties": { "kind": { "$ref": "#/definitions/isDocker" } },
            "required": ["kind"]
        },
        "then": { "properties": { "image": { "description": "then branch" } } },
        "else": { "properties": { "image": { "description": "else branch" } } },
        "definitions": { "isDocker": { "const": "docker" } }
    });

    for (kind, expected) in [("docker", "then branch"), ("podman", "else branch")] {
        let (schemas, url) = seeded(schema.clone()).await;

        let found = schemas
            .schemas_at_path(
                &url,
                &json!({ "kind": kind }),
                &"image".parse::<Keys>().unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(descriptions(&found), [expected], "for kind = {kind}");
    }
}

/// A reference the document cannot supply leaves the condition undecided, so
/// both branches are offered. Asserted through `schemas_at_path`, because
/// `is_valid` reports plain `false` for this and deciding by it would pick
/// `else` silently.
#[tokio::test]
async fn a_condition_holding_an_unresolvable_reference_takes_both_branches() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": {
            "properties": {
                "kind": { "$ref": "file:///taplo-test/missing.json#/definitions/isDocker" }
            },
            "required": ["kind"]
        },
        "then": { "properties": { "image": { "description": "then branch" } } },
        "else": { "properties": { "image": { "description": "else branch" } } }
    }))
    .await;

    let found = schemas
        .schemas_at_path(
            &url,
            &json!({ "kind": "docker" }),
            &"image".parse::<Keys>().unwrap(),
        )
        .await
        .unwrap();

    let mut found = descriptions(&found);
    found.sort_unstable();
    assert_eq!(found, ["else branch", "then branch"]);
}

#[tokio::test]
async fn a_condition_holding_a_dangling_pointer_takes_both_branches() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": {
            "properties": { "kind": { "$ref": "#/definitions/nope" } },
            "required": ["kind"]
        },
        "then": { "properties": { "image": { "description": "then branch" } } },
        "else": { "properties": { "image": { "description": "else branch" } } },
        "definitions": {}
    }))
    .await;

    let found = schemas
        .schemas_at_path(
            &url,
            &json!({ "kind": "docker" }),
            &"image".parse::<Keys>().unwrap(),
        )
        .await
        .unwrap();

    let mut found = descriptions(&found);
    found.sort_unstable();
    assert_eq!(found, ["else branch", "then branch"]);
}

#[tokio::test]
async fn a_condition_that_does_not_compile_takes_both_branches() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": { "pattern": "(" },
        "then": { "properties": { "image": { "description": "then" } } },
        "else": { "properties": { "image": { "description": "else" } } }
    }))
    .await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "docker" }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["then", "else"]);
}

#[tokio::test]
async fn a_draft_4_root_still_decides_a_const_condition() {
    let mut schema = conditional_schema();
    schema["$schema"] = json!("http://json-schema.org/draft-04/schema#");
    let (schemas, url) = seeded(schema).await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "podman" }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["else"]);
}

#[tokio::test]
async fn branches_without_a_condition_are_inert() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "then": { "properties": { "image": { "description": "then" } } },
        "else": { "properties": { "image": { "description": "else" } } }
    }))
    .await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({}), &keys)
        .await
        .unwrap();

    assert!(found.is_empty());
}

#[tokio::test]
async fn the_schema_carrying_a_condition_is_still_collected() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "server": {
                "description": "carrier",
                "if": { "properties": { "kind": { "const": "docker" } }, "required": ["kind"] },
                "then": { "description": "then" }
            }
        }
    }))
    .await;

    let keys = "server".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "server": { "kind": "docker" } }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["then", "carrier"]);
}

#[tokio::test]
async fn completion_follows_the_branch_the_document_selects() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": { "properties": { "kind": { "const": "docker" } }, "required": ["kind"] },
        "then": { "properties": { "image": { "type": "string" } } },
        "else": { "properties": { "binary": { "type": "string" } } }
    }))
    .await;

    let children = schemas
        .possible_schemas_from(&url, &json!({ "kind": "docker" }), &Keys::empty(), 5)
        .await
        .unwrap();

    let mut offered: Vec<String> = children
        .iter()
        .map(|(_, relative, _)| relative.to_string())
        .filter(|key| !key.is_empty())
        .collect();
    offered.sort();

    assert_eq!(offered, ["image"]);
}

/// One dependent schema under whichever keyword spells it, so that both
/// spellings are asserted against the same shape.
fn dependent_schema(keyword: &str) -> Value {
    json!({
        "type": "object",
        keyword: {
            "kind": { "properties": { "image": { "description": "dependent" } } }
        }
    })
}

#[tokio::test]
async fn a_present_trigger_key_applies_its_dependent_schema() {
    for keyword in ["dependencies", "dependentSchemas"] {
        let (schemas, url) = seeded(dependent_schema(keyword)).await;

        let keys = "image".parse::<Keys>().unwrap();
        let found = schemas
            .schemas_at_path(&url, &json!({ "kind": "docker" }), &keys)
            .await
            .unwrap();

        assert_eq!(descriptions(&found), ["dependent"], "keyword {keyword}");
    }
}

#[tokio::test]
async fn an_absent_trigger_key_applies_nothing() {
    for keyword in ["dependencies", "dependentSchemas"] {
        let (schemas, url) = seeded(dependent_schema(keyword)).await;

        let keys = "image".parse::<Keys>().unwrap();
        let found = schemas
            .schemas_at_path(&url, &json!({ "other": 1 }), &keys)
            .await
            .unwrap();

        assert!(found.is_empty(), "keyword {keyword}");
    }
}

#[tokio::test]
async fn an_absent_instance_applies_every_dependent_schema() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "dependentSchemas": {
            "a": { "properties": { "image": { "description": "from a" } } },
            "b": { "properties": { "image": { "description": "from b" } } }
        }
    }))
    .await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["from a", "from b"]);
}

#[tokio::test]
async fn the_array_form_of_dependencies_applies_nothing() {
    for keyword in ["dependencies", "dependentRequired"] {
        let (schemas, url) = seeded(json!({
            "type": "object",
            keyword: { "kind": ["image"] }
        }))
        .await;

        let keys = "image".parse::<Keys>().unwrap();
        let found = schemas
            .schemas_at_path(&url, &json!({ "kind": "docker" }), &keys)
            .await
            .unwrap();

        assert!(found.is_empty(), "keyword {keyword}");
    }
}

#[tokio::test]
async fn an_unevaluated_key_falls_back_to_unevaluated_properties() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": { "known": { "description": "known" } },
        "unevaluatedProperties": { "description": "unevaluated" }
    }))
    .await;

    let keys = "other".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "other": 1 }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["unevaluated"]);
}

#[tokio::test]
async fn an_evaluated_key_does_not_fall_back() {
    let cases = [
        json!({ "properties": { "known": { "description": "known" } } }),
        json!({ "patternProperties": { "^kn": { "description": "known" } } }),
        json!({ "additionalProperties": { "description": "known" } }),
        json!({ "allOf": [{ "properties": { "known": { "description": "known" } } }] }),
        json!({ "oneOf": [{ "properties": { "known": { "description": "known" } } }] }),
        json!({ "anyOf": [{ "properties": { "known": { "description": "known" } } }] }),
        json!({
            "if": { "properties": { "kind": { "const": "a" } }, "required": ["kind"] },
            "then": { "properties": { "known": { "description": "known" } } }
        }),
        json!({
            "dependentSchemas": {
                "kind": { "properties": { "known": { "description": "known" } } }
            }
        }),
    ];

    for case in cases {
        let mut schema = case.clone();
        schema["type"] = json!("object");
        schema["unevaluatedProperties"] = json!({ "description": "unevaluated" });

        let (schemas, url) = seeded(schema).await;

        let keys = "known".parse::<Keys>().unwrap();
        let found = schemas
            .schemas_at_path(&url, &json!({ "kind": "a", "known": 1 }), &keys)
            .await
            .unwrap();

        assert_eq!(descriptions(&found), ["known"], "case {case}");
    }
}

#[tokio::test]
async fn a_key_only_the_unselected_branch_evaluates_falls_back() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": { "properties": { "kind": { "const": "a" } }, "required": ["kind"] },
        "then": { "properties": { "other": { "description": "then" } } },
        "unevaluatedProperties": { "description": "unevaluated" }
    }))
    .await;

    let keys = "other".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "b", "other": 1 }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["unevaluated"]);
}

#[tokio::test]
async fn a_deep_path_asks_about_its_head_key() {
    let cases = [
        json!({ "allOf": [{ "properties": { "a": { "type": "object" } } }] }),
        json!({ "additionalProperties": false }),
    ];

    for case in cases {
        let mut schema = case.clone();
        schema["type"] = json!("object");
        schema["unevaluatedProperties"] =
            json!({ "properties": { "b": { "description": "unevaluated" } } });

        let (schemas, url) = seeded(schema).await;

        let keys = "a.b".parse::<Keys>().unwrap();
        let found = schemas
            .schemas_at_path(&url, &json!({ "a": { "b": 1 } }), &keys)
            .await
            .unwrap();

        assert!(found.is_empty(), "case {case}");
    }
}

#[tokio::test]
async fn a_boolean_unevaluated_properties_yields_nothing() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": { "known": { "description": "known" } },
        "unevaluatedProperties": false
    }))
    .await;

    let keys = "other".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "other": 1 }), &keys)
        .await
        .unwrap();

    assert!(found.is_empty());
}

#[tokio::test]
async fn a_negated_subschema_is_never_traversed() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "not": { "properties": { "banned": { "description": "banned" } } }
    }))
    .await;

    let keys = "banned".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "banned": 1 }), &keys)
        .await
        .unwrap();
    assert!(found.is_empty());

    let children = schemas
        .possible_schemas_from(&url, &json!({}), &Keys::empty(), 5)
        .await
        .unwrap();
    assert!(children
        .iter()
        .all(|(_, relative, _)| relative.to_string() != "banned"));
}

#[tokio::test]
async fn property_names_is_never_the_schema_for_a_value() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "propertyNames": { "pattern": "^[a-z]+$", "description": "names" }
    }))
    .await;

    let keys = "abc".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "abc": 1 }), &keys)
        .await
        .unwrap();

    assert!(found.is_empty());
}

#[tokio::test]
async fn contains_is_never_the_schema_for_an_index() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "list": {
                "type": "array",
                "contains": { "type": "string", "description": "contained" }
            }
        }
    }))
    .await;

    let keys = "list".parse::<Keys>().unwrap().join(0_usize);
    let found = schemas
        .schemas_at_path(&url, &json!({ "list": ["x"] }), &keys)
        .await
        .unwrap();

    assert!(found.is_empty());
}

/// Runs an async traversal on its own thread and runtime and panics with
/// `what` when it has not finished inside `bound`.
///
/// `tokio::time::timeout` cannot bound a traversal: it never reaches an await
/// point that yields to the runtime, so the timeout future never gets to run.
fn assert_finishes_within<T, F, Fut>(bound: std::time::Duration, what: &'static str, work: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T>,
{
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        drop(tx.send(runtime.block_on(work())));
    });

    rx.recv_timeout(bound).unwrap_or_else(|_| panic!("{what}"))
}

/// The composed-`allOf` branch merges its members instead of recursing on
/// `$ref`, so a cycle whose nodes are reachable only as members is invisible to
/// a check that consults the visited set without ever feeding it.
#[test]
fn a_cycle_through_composed_all_of_members_terminates() {
    assert_finishes_within(
        std::time::Duration::from_secs(5),
        "a cycle through merged allOf members did not terminate",
        || async {
            let (schemas, url) = seeded(json!({
                "type": "object",
                "properties": { "a": { "$ref": "#/definitions/a" } },
                "definitions": {
                    "a": { "allOf": [{ "$ref": "#/definitions/b" }] },
                    "b": { "allOf": [
                        { "$ref": "#/definitions/c" },
                        { "$ref": "#/definitions/d" },
                        { "$ref": "#/definitions/e" }
                    ] },
                    "c": { "allOf": [{ "$ref": "#/definitions/b" }] },
                    "d": { "allOf": [{ "$ref": "#/definitions/b" }] },
                    "e": { "allOf": [{ "$ref": "#/definitions/b" }] }
                }
            }))
            .await;

            schemas
                .possible_schemas_from(&url, &Value::Null, &Keys::empty(), 5)
                .await
                .map(|found| found.len())
        },
    )
    .unwrap();
}

#[test]
fn a_composed_all_of_cycle_terminates() {
    assert_finishes_within(
        std::time::Duration::from_secs(5),
        "a composed allOf cycle did not terminate",
        || async {
            let (schemas, url) = seeded(json!({
                "type": "object",
                "properties": { "node": { "$ref": "#/definitions/node" } },
                "definitions": {
                    "node": { "allOf": [
                        { "$ref": "#/definitions/node" },
                        { "$ref": "#/definitions/node" },
                        { "$ref": "#/definitions/node" }
                    ] }
                }
            }))
            .await;

            schemas
                .possible_schemas_from(&url, &Value::Null, &Keys::empty(), 5)
                .await
                .map(|found| found.len())
        },
    )
    .unwrap();
}

#[test]
fn a_three_way_any_of_cycle_terminates() {
    assert_finishes_within(
        std::time::Duration::from_secs(5),
        "a three-way anyOf cycle did not terminate",
        || async {
            let (schemas, url) = seeded(json!({
                "type": "object",
                "properties": { "node": { "$ref": "#/definitions/node" } },
                "definitions": {
                    "node": { "anyOf": [
                        { "$ref": "#/definitions/node" },
                        { "$ref": "#/definitions/node" },
                        { "$ref": "#/definitions/node" }
                    ] }
                }
            }))
            .await;

            let keys = "node".parse::<Keys>().unwrap();

            schemas
                .schemas_at_path(&url, &Value::Null, &keys)
                .await
                .map(|found| found.len())
        },
    )
    .unwrap();
}

/// Seeds several documents into the in-memory cache and returns the URL of the
/// first, which is the root. Paths are relative to `file:///taplo-test/`.
async fn seeded_documents(documents: &[(&str, Value)]) -> (Schemas<NativeEnvironment>, Url) {
    let schemas = Schemas::new(NativeEnvironment::new(), reqwest::Client::new());
    let base = Url::parse("file:///taplo-test/").unwrap();

    let mut root = None;

    for (path, document) in documents {
        let url = base.join(path).unwrap();
        drop(
            schemas
                .cache()
                .store(url.clone(), Arc::new(document.clone()))
                .await,
        );
        root.get_or_insert(url);
    }

    (schemas, root.expect("no documents seeded"))
}

#[tokio::test]
async fn a_relative_reference_resolves_against_the_document() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": { "port": { "$ref": "common.json#/definitions/port" } }
            }),
        ),
        (
            "common.json",
            json!({
                "definitions": { "port": { "description": "relative", "type": "integer" } }
            }),
        ),
    ])
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["relative"]);
}

#[tokio::test]
async fn a_reference_walks_out_of_the_document_directory() {
    let (schemas, url) = seeded_documents(&[
        (
            "nested/schema.json",
            json!({
                "type": "object",
                "properties": {
                    "up": { "$ref": "../common.json#/definitions/port" },
                    "rooted": { "$ref": "/taplo-test/common.json#/definitions/port" }
                }
            }),
        ),
        (
            "common.json",
            json!({
                "definitions": { "port": { "description": "shared", "type": "integer" } }
            }),
        ),
    ])
    .await;

    for key in ["up", "rooted"] {
        let found = schemas
            .schemas_at_path(&url, &Value::Null, &key.parse::<Keys>().unwrap())
            .await
            .unwrap();
        assert_eq!(descriptions(&found), ["shared"], "at `{key}`");
    }
}

#[tokio::test]
async fn an_absolute_reference_carrying_a_pointer_resolves() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": {
                    "port": { "$ref": "file:///taplo-test/common.json#/definitions/port" }
                }
            }),
        ),
        (
            "common.json",
            json!({
                "definitions": { "port": { "description": "absolute", "type": "integer" } }
            }),
        ),
    ])
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["absolute"]);
}

#[tokio::test]
async fn a_reference_to_the_whole_document_resolves() {
    let (schemas, url) = seeded(json!({
        "description": "the root",
        "type": "object",
        "properties": { "child": { "$ref": "#" } }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"child".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["the root"]);
}

#[tokio::test]
async fn a_fragment_is_percent_decoded_and_unescaped() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "spaced": { "$ref": "#/definitions/two words" },
            "slashed": { "$ref": "#/definitions/a~1b" }
        },
        "definitions": {
            "two words": { "description": "spaced", "type": "integer" },
            "a/b": { "description": "slashed", "type": "integer" }
        }
    }))
    .await;

    for (key, expected) in [("spaced", "spaced"), ("slashed", "slashed")] {
        let found = schemas
            .schemas_at_path(&url, &Value::Null, &key.parse::<Keys>().unwrap())
            .await
            .unwrap();
        assert_eq!(descriptions(&found), [expected], "at `{key}`");
    }
}

#[test]
fn a_relative_reference_under_an_https_base_joins_to_https() {
    let base = Url::parse("https://example.com/schemas/root.json").unwrap();

    assert_eq!(
        reference_url(&base, "common.json#/definitions/port")
            .unwrap()
            .as_str(),
        "https://example.com/schemas/common.json#/definitions/port"
    );
}

/// A document that references itself at every level, queried deep enough that
/// the branching would show if the visited set were not consulted. It belongs
/// here rather than with the other cycle tests, because `$ref: "#"` does not
/// resolve at all until this task lands.
#[test]
fn a_self_referential_document_terminates_at_depth() {
    assert_finishes_within(
        std::time::Duration::from_secs(5),
        "a self-referential document did not terminate at depth 8",
        || async {
            let (schemas, url) = seeded(json!({
                "anyOf": [{ "$ref": "#" }, { "$ref": "#" }],
                "properties": { "a": { "$ref": "#" } }
            }))
            .await;

            let keys = "a.a.a.a.a.a.a.a".parse::<Keys>().unwrap();

            schemas
                .schemas_at_path(&url, &Value::Null, &keys)
                .await
                .map(|found| found.len())
        },
    )
    .unwrap();
}

/// Traversal applies a fragment as a JSON pointer to the raw document, so the
/// name of the definition container is a key like any other and no draft is
/// consulted. Pinned so a later change to fragment handling cannot break it
/// quietly.
#[tokio::test]
async fn defs_and_definitions_resolve_under_every_draft() {
    for meta in [
        "http://json-schema.org/draft-07/schema#",
        "https://json-schema.org/draft/2019-09/schema",
        "https://json-schema.org/draft/2020-12/schema",
    ] {
        let (schemas, url) = seeded(json!({
            "$schema": meta,
            "type": "object",
            "properties": {
                "new": { "$ref": "#/$defs/port" },
                "old": { "$ref": "#/definitions/port" }
            },
            "$defs": { "port": { "description": "defs", "type": "integer" } },
            "definitions": { "port": { "description": "definitions", "type": "integer" } }
        }))
        .await;

        for (key, expected) in [("new", "defs"), ("old", "definitions")] {
            let found = schemas
                .schemas_at_path(&url, &Value::Null, &key.parse::<Keys>().unwrap())
                .await
                .unwrap();
            assert_eq!(descriptions(&found), [expected], "at `{key}` under {meta}");
        }
    }
}

#[tokio::test]
async fn an_inner_id_rebases_the_references_beneath_it() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": { "port": { "$ref": "#/definitions/wrapper" } },
                "definitions": {
                    "wrapper": { "$id": "defs/", "$ref": "port.json" }
                }
            }),
        ),
        (
            "defs/port.json",
            json!({ "description": "rescoped", "type": "integer" }),
        ),
    ])
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["rescoped"]);
}

#[tokio::test]
async fn a_reference_rebases_at_the_document_it_reaches() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": { "port": { "$ref": "sub/inner.json#/definitions/port" } }
            }),
        ),
        (
            "sub/inner.json",
            json!({ "definitions": { "port": { "$ref": "port.json" } } }),
        ),
        (
            "sub/port.json",
            json!({ "description": "beside inner", "type": "integer" }),
        ),
    ])
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["beside inner"]);
}

/// A pointer collects an `$id` from every object it walks through, which is
/// what `jsonschema`'s own `resolve_fragment` does through `join_folders`.
#[tokio::test]
async fn a_pointer_crossing_an_id_rebases_beneath_it() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": { "port": { "$ref": "#/definitions/sub/properties/x" } },
                "definitions": {
                    "sub": {
                        "$id": "sub/",
                        "properties": { "x": { "$ref": "port.json" } }
                    }
                }
            }),
        ),
        (
            "sub/port.json",
            json!({ "description": "under sub", "type": "integer" }),
        ),
    ])
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["under sub"]);
}

#[tokio::test]
async fn a_plain_name_fragment_resolves_to_the_id_that_claims_it() {
    let (schemas, url) = seeded(json!({
        "$id": "file:///taplo-test/schema.json",
        "type": "object",
        "properties": { "port": { "$ref": "#port" } },
        "definitions": {
            "port": { "$id": "#port", "description": "anchored", "type": "integer" }
        }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["anchored"]);
}

#[tokio::test]
async fn an_anchor_keyword_resolves_in_both_halves() {
    let (schemas, url) = seeded(json!({
        "$id": "file:///taplo-test/schema.json",
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": { "port": { "$ref": "#port" } },
        "$defs": {
            "port": { "$anchor": "port", "description": "anchored", "type": "integer" }
        }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();
    assert_eq!(descriptions(&found), ["anchored"]);

    let errors = schemas
        .validate(&url, &json!({ "port": "not an integer" }))
        .await
        .unwrap();

    assert!(
        errors.iter().any(|e| matches!(
            e.kind(),
            jsonschema::error::ValidationErrorKind::Type { .. }
        )),
        "expected a type error, got {errors:?}"
    );
}

#[tokio::test]
async fn a_sibling_description_wins_over_the_target() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "port": { "$ref": "#/definitions/port", "description": "the sibling" }
        },
        "definitions": {
            "port": { "description": "the target", "type": "integer" }
        }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["the sibling"]);
    assert_eq!(found[0].1["type"], "integer");
    assert_eq!(found.len(), 1, "the carrier and the target are one schema");
}

#[tokio::test]
async fn a_sibling_enum_replaces_the_target_enum() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": { "kind": { "$ref": "#/definitions/kind", "enum": ["a"] } },
        "definitions": { "kind": { "type": "string", "enum": ["b", "c"] } }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"kind".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(found[0].1["enum"], json!(["a"]));
}

#[tokio::test]
async fn a_sibling_required_unions_with_the_target() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "server": { "$ref": "#/definitions/server", "required": ["extra"] }
        },
        "definitions": { "server": { "type": "object", "required": ["name"] } }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"server".parse::<Keys>().unwrap())
        .await
        .unwrap();

    let required = found[0].1["required"].as_array().unwrap();
    assert!(required.contains(&json!("name")) && required.contains(&json!("extra")));
}

#[tokio::test]
async fn sibling_properties_union_with_the_target() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": {
                    "server": {
                        "$ref": "sub/server.json",
                        "properties": { "extra": { "$ref": "#/definitions/extra" } }
                    }
                },
                "definitions": { "extra": { "description": "sibling extra", "type": "string" } }
            }),
        ),
        (
            "sub/server.json",
            json!({
                "type": "object",
                "properties": { "name": { "description": "target name", "type": "string" } }
            }),
        ),
    ])
    .await;

    for (path, expected) in [
        ("server.extra", "sibling extra"),
        ("server.name", "target name"),
    ] {
        let found = schemas
            .schemas_at_path(&url, &Value::Null, &path.parse::<Keys>().unwrap())
            .await
            .unwrap();
        assert_eq!(descriptions(&found), [expected], "at `{path}`");
    }
}

#[tokio::test]
async fn a_sibling_unevaluated_properties_applies() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "obj": {
                "$ref": "#/definitions/obj",
                "unevaluatedProperties": { "description": "anything else", "type": "string" }
            }
        },
        "definitions": {
            "obj": { "type": "object", "properties": { "known": { "type": "string" } } }
        }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"obj.unknown".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["anything else"]);
}

/// A composed-`allOf` member is merged rather than recursed into, so it never
/// reaches the entry that re-bases. Its own pointers have to be made absolute
/// against its own document before the merge, or they resolve against the
/// carrier's.
#[tokio::test]
async fn a_composed_all_of_member_keeps_its_own_document() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": {
                    "server": {
                        "description": "carrier",
                        "allOf": [{ "$ref": "sub/server.json" }]
                    }
                }
            }),
        ),
        (
            "sub/server.json",
            json!({
                "type": "object",
                "properties": { "name": { "$ref": "#/definitions/name" } },
                "definitions": {
                    "name": { "description": "member name", "type": "string" }
                }
            }),
        ),
    ])
    .await;

    let children = schemas
        .possible_schemas_from(&url, &Value::Null, &Keys::empty(), 5)
        .await
        .unwrap();

    let name = schema_at(&children, "server.name").expect("no schema for `server.name`");
    assert_eq!(name["description"], "member name", "got {name}");
}

/// `$defs` beside a `$ref` is not an applicable sibling: it is where the
/// reference points, not a keyword describing the instance. The root shape
/// `pydantic` emits stays on the fast path.
#[tokio::test]
async fn a_defs_container_beside_a_ref_is_not_a_sibling() {
    let (schemas, url) = seeded(json!({
        "$ref": "#/$defs/model",
        "$defs": {
            "model": {
                "type": "object",
                "properties": { "port": { "description": "the model", "type": "integer" } }
            }
        }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["the model"]);
}

/// Every shape in the spec's end-to-end table, asserted of both halves at once.
/// A validation result of "no errors" is not acceptable: an unresolved
/// reference produces exactly that, so the type error has to be present and
/// the resolver errors absent.
#[tokio::test]
async fn traversal_and_validation_agree_on_every_reference_shape() {
    let shapes: &[(&str, Value)] = &[
        ("pointer", json!({ "$ref": "#/definitions/port" })),
        ("defs-pointer", json!({ "$ref": "#/$defs/port" })),
        (
            "relative",
            json!({ "$ref": "common.json#/definitions/port" }),
        ),
        (
            "absolute",
            json!({ "$ref": "file:///taplo-test/common.json#/definitions/port" }),
        ),
        ("anchor", json!({ "$ref": "#port" })),
        ("rescoped", json!({ "$ref": "#/definitions/wrapper" })),
    ];

    for (name, reference) in shapes {
        let (schemas, url) = seeded_documents(&[
            (
                "schema.json",
                json!({
                    "type": "object",
                    "properties": { "port": reference },
                    "definitions": {
                        "port": { "description": name, "type": "integer" },
                        "anchored": { "$id": "#port", "description": name, "type": "integer" },
                        "wrapper": { "$id": "defs/", "$ref": "port.json" }
                    },
                    "$defs": { "port": { "description": name, "type": "integer" } }
                }),
            ),
            (
                "common.json",
                json!({
                    "definitions": { "port": { "description": name, "type": "integer" } }
                }),
            ),
            (
                "defs/port.json",
                json!({ "description": name, "type": "integer" }),
            ),
        ])
        .await;

        let found = schemas
            .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
            .await
            .unwrap_or_else(|e| panic!("traversal failed for `{name}`: {e}"));

        assert_eq!(descriptions(&found), [*name], "traversal, `{name}`");

        let errors = schemas
            .validate(&url, &json!({ "port": "not an integer" }))
            .await
            .unwrap_or_else(|e| panic!("validation failed for `{name}`: {e:#}"));

        assert!(
            errors.iter().any(|e| matches!(
                e.kind(),
                jsonschema::error::ValidationErrorKind::Type { .. }
            )),
            "validation, `{name}`: expected a type error, got {errors:?}"
        );

        assert!(
            !errors.iter().any(|e| matches!(
                e.kind(),
                jsonschema::error::ValidationErrorKind::Referencing(_)
            )),
            "validation, `{name}`: a reference did not resolve, {errors:?}"
        );
    }
}

#[tokio::test]
async fn a_relative_root_id_does_not_break_compilation() {
    let (schemas, url) = seeded(json!({
        "$id": "schema.json",
        "type": "object",
        "properties": { "port": { "type": "integer" } }
    }))
    .await;

    let errors = schemas
        .validate(&url, &json!({ "port": "not an integer" }))
        .await
        .expect("compilation failed for a relative root $id");

    assert_eq!(errors.len(), 1);
}

#[tokio::test]
async fn a_draft_4_root_resolves_a_relative_reference() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "type": "object",
                "properties": { "port": { "$ref": "common.json#/definitions/port" } }
            }),
        ),
        (
            "common.json",
            json!({ "definitions": { "port": { "type": "integer" } } }),
        ),
    ])
    .await;

    let errors = schemas
        .validate(&url, &json!({ "port": "not an integer" }))
        .await
        .expect("validation errored for a draft-4 root");

    assert!(
        errors.iter().any(|e| matches!(
            e.kind(),
            jsonschema::error::ValidationErrorKind::Type { .. }
        )),
        "expected a type error, got {errors:?}"
    );
}
