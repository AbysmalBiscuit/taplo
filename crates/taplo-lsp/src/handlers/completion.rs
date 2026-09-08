use lsp_async_stub::{
    rpc::Error,
    util::{LspExt, Position},
    Context, Params,
};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionItemTag, CompletionParams, CompletionResponse,
    CompletionTextEdit, Documentation, InsertTextFormat, MarkupContent, Range, TextEdit,
};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Write as _;
use taplo::dom::{node::TableKind, Keys, Node};
use taplo_common::{
    environment::Environment,
    schema::{ext::schema_ext_of, ValueExt},
};

use crate::{
    query::{lookup_keys, Query},
    world::World,
};

#[tracing::instrument(skip_all)]
pub async fn completion<E: Environment>(
    context: Context<World<E>>,
    params: Params<CompletionParams>,
) -> Result<Option<CompletionResponse>, Error> {
    let p = params.required()?;

    let document_uri = p.text_document_position.text_document.uri;

    let workspaces = context.workspaces.read().await;
    let ws = workspaces.by_document(&document_uri);

    // All completions are tied to schemas.
    if !ws.config.schema.enabled {
        return Ok(None);
    }

    let doc = match ws.document(&document_uri) {
        Ok(d) => d,
        Err(error) => {
            tracing::debug!(%error, "failed to get document from workspace");
            return Ok(None);
        }
    };

    let Some(schema_association) = ws.schemas.associations().association_for(&document_uri) else {
        return Ok(None);
    };

    let position = p.text_document_position.position;
    let Some(offset) = doc.mapper.offset(Position::from_lsp(position)) else {
        tracing::error!(?position, "document position not found");
        return Ok(None);
    };

    let query = Query::at(&doc.dom, offset);

    let value = match serde_json::to_value(&doc.dom) {
        Ok(v) => v,
        Err(error) => {
            tracing::warn!(%error, "unable to serialize DOM");
            Value::Null
        }
    };

    if query.in_table_header() {
        let key_count = query.header_keys().len();

        let object_schemas = match ws
            .schemas
            .possible_schemas_from(
                &schema_association.url,
                &value,
                &Keys::empty(),
                key_count + ws.config.completion.max_keys + 1,
            )
            .await
            .map(|s| {
                s.into_iter().filter(|(_, _, s)| {
                    s["type"].is_null()
                        || s["type"] == "object"
                        || s["type"]
                            .as_array()
                            .is_some_and(|arr| arr.iter().any(|v| v == "object"))
                })
            }) {
            Ok(s) => s,
            Err(error) => {
                tracing::error!(?error, "failed to collect schemas");
                return Ok(None);
            }
        };

        let key_range = query.header_key().map(|k| k.text_range()).and_then(|r| {
            if r.is_empty() {
                None
            } else {
                Some(r)
            }
        });

        let node = query
            .dom_node()
            .cloned()
            .unwrap_or_else(|| (Keys::empty(), doc.dom.clone()));

        return Ok(Some(CompletionResponse::Array(
            object_schemas
                // Filter out existing tables in the dom.
                .filter(|(full_key, _, _)| match doc.dom.path(full_key) {
                    Some(n) => {
                        node.0 == *full_key
                            || n.as_table().is_some_and(|t| t.kind() == TableKind::Pseudo)
                    }
                    None => true,
                })
                .map(|(full_key, _, s)| CompletionItem {
                    label: full_key.to_string(),
                    kind: Some(CompletionItemKind::STRUCT),
                    text_edit: key_range.map(|r| {
                        CompletionTextEdit::Edit(TextEdit {
                            range: doc.mapper.range(r).unwrap().into_lsp(),
                            new_text: full_key.to_string(),
                        })
                    }),
                    ..schema_annotated_item(&s)
                })
                .collect(),
        )));
    }

    if query.in_table_array_header() {
        let key_count = query.header_keys().len();
        let array_of_objects_schemas = match ws
            .schemas
            .possible_schemas_from(
                &schema_association.url,
                &value,
                &Keys::empty(),
                key_count + ws.config.completion.max_keys + 1,
            )
            .await
            .map(|s| {
                s.into_iter().filter(|(_, _, s)| {
                    s["type"] == "array"
                        && (s["items"]["type"] == "object" || s["items"]["type"].is_null())
                })
            }) {
            Ok(s) => s,
            Err(error) => {
                tracing::error!(?error, "failed to collect schemas");
                return Ok(None);
            }
        };

        let key_range = query.header_key().map(|k| k.text_range()).and_then(|r| {
            if r.is_empty() {
                None
            } else {
                Some(r)
            }
        });

        return Ok(Some(CompletionResponse::Array(
            array_of_objects_schemas
                .map(|(full_key, _, s)| CompletionItem {
                    label: full_key.to_string(),
                    kind: Some(CompletionItemKind::STRUCT),
                    text_edit: key_range.map(|r| {
                        CompletionTextEdit::Edit(TextEdit {
                            range: doc.mapper.range(r).unwrap().into_lsp(),
                            new_text: full_key.to_string(),
                        })
                    }),
                    ..schema_annotated_item(&s)
                })
                .collect(),
        )));
    }

    if query.empty_line() {
        let parent_table = query.parent_table_or_array_table(&doc.dom);

        let schemas = match ws
            .schemas
            .possible_schemas_from(
                &schema_association.url,
                &value,
                &lookup_keys(doc.dom.clone(), &parent_table.0),
                ws.config.completion.max_keys + 1,
            )
            .await
        {
            Ok(s) => s,
            Err(error) => {
                tracing::error!(?error, "failed to collect schemas");
                return Ok(None);
            }
        };

        return Ok(Some(CompletionResponse::Array(
            schemas
                .into_iter()
                // Filter out existing items.
                .filter(|(full_key, _, _)| match doc.dom.path(full_key) {
                    Some(n) => n.as_table().is_some_and(|t| t.kind() == TableKind::Pseudo),
                    None => true,
                })
                .map(|(_, relative_keys, schema)| CompletionItem {
                    label: relative_keys.to_string(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    insert_text: Some(new_entry_snippet(&relative_keys, &schema, false)),
                    ..schema_annotated_item(&schema)
                })
                .collect(),
        )));
    }

    if query.in_entry_keys() {
        let mut parent_keys = if let Some((k, _)) = query.dom_node() {
            k.clone()
        } else {
            query.parent_table_or_array_table(&doc.dom).0
        };

        let entry_keys = query.entry_keys();

        parent_keys = parent_keys.skip_right(entry_keys.len());

        let schemas = match ws
            .schemas
            .possible_schemas_from(
                &schema_association.url,
                &value,
                &lookup_keys(doc.dom.clone(), &parent_keys),
                entry_keys.len() + ws.config.completion.max_keys + 1,
            )
            .await
        {
            Ok(s) => s,
            Err(error) => {
                tracing::error!(?error, "failed to collect schemas");
                return Ok(None);
            }
        };

        let key_range = query.entry_key().map(|k| k.text_range());

        let has_eq = query.entry_has_eq();

        return Ok(Some(CompletionResponse::Array(
            schemas
                .into_iter()
                .map(|(_, relative_keys, schema)| CompletionItem {
                    label: relative_keys.to_string(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    text_edit: key_range.map(|r| {
                        CompletionTextEdit::Edit(TextEdit {
                            range: doc.mapper.range(r).unwrap().into_lsp(),
                            new_text: if has_eq {
                                relative_keys.to_string() + " "
                            } else {
                                new_entry_snippet(&relative_keys, &schema, false)
                            },
                        })
                    }),
                    insert_text: Some(if has_eq {
                        relative_keys.to_string() + " "
                    } else {
                        new_entry_snippet(&relative_keys, &schema, false)
                    }),
                    insert_text_format: if has_eq {
                        None
                    } else {
                        Some(InsertTextFormat::SNIPPET)
                    },
                    ..schema_annotated_item(&schema)
                })
                .collect(),
        )));
    }

    if query.in_entry_value() {
        let (path, _) = query.dom_node().unwrap();

        // Pretty much same as the entry on an empty line
        if query.in_inline_table() {
            let schemas = match ws
                .schemas
                .possible_schemas_from(
                    &schema_association.url,
                    &value,
                    &lookup_keys(doc.dom.clone(), path),
                    ws.config.completion.max_keys + 1,
                )
                .await
            {
                Ok(s) => s,
                Err(error) => {
                    tracing::error!(?error, "failed to collect schemas");
                    return Ok(None);
                }
            };

            return Ok(Some(CompletionResponse::Array(
                schemas
                    .into_iter()
                    // Filter out existing items.
                    .filter(|(full_key, _, _)| match doc.dom.path(full_key) {
                        Some(n) => n.as_table().is_some_and(|t| t.kind() == TableKind::Pseudo),
                        None => true,
                    })
                    .map(|(_, relative_keys, schema)| CompletionItem {
                        label: relative_keys.to_string(),
                        kind: Some(CompletionItemKind::VARIABLE),
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        insert_text: Some(new_entry_snippet(&relative_keys, &schema, false)),
                        ..schema_annotated_item(&schema)
                    })
                    .collect(),
            )));
        }

        let path = if query.is_inline() {
            lookup_keys(doc.dom.clone(), &path.clone())
        } else {
            let parent = query.parent_table_or_array_table(&doc.dom);
            let entry_key = query.entry_keys();
            lookup_keys(doc.dom.clone(), &parent.0.extend(entry_key))
        };

        let schemas = match ws
            .schemas
            .possible_schemas_from(
                &schema_association.url,
                &value,
                &path,
                ws.config.completion.max_keys + 1,
            )
            .await
        {
            Ok(s) => s,
            Err(error) => {
                tracing::error!(?error, "failed to collect schemas");
                return Ok(None);
            }
        };

        let range = if query.in_array() {
            None
        } else {
            query
                .entry_value()
                .map(|k| k.text_range())
                .and_then(|r| doc.mapper.range(r))
                .map(lsp_async_stub::util::LspExt::into_lsp)
        };

        let mut completions = Vec::new();

        for (_, _, schema) in schemas {
            add_value_completions(
                &schema,
                range,
                &mut completions,
                query.is_single_quote_value(),
            );
        }

        return Ok(Some(CompletionResponse::Array(completions)));
    }

    // Only standalone keys left.
    // Almost the same as an empty line except we need to replace the incomplete keys.
    let mut parent_keys = if let Some((k, _)) = query.dom_node() {
        k.clone()
    } else {
        query.parent_table_or_array_table(&doc.dom).0
    };

    let entry_keys = query.entry_keys();

    parent_keys = parent_keys.skip_right(entry_keys.len());

    let schemas = match ws
        .schemas
        .possible_schemas_from(
            &schema_association.url,
            &value,
            &lookup_keys(doc.dom.clone(), &parent_keys),
            ws.config.completion.max_keys + 1,
        )
        .await
    {
        Ok(s) => s,
        Err(error) => {
            tracing::error!(?error, "failed to collect schemas");
            return Ok(None);
        }
    };

    Ok(Some(CompletionResponse::Array(
        schemas
            .into_iter()
            // Filter out existing items.
            .filter(|(full_key, _, _)| match doc.dom.path(full_key) {
                Some(n) => n.as_table().is_some_and(|t| t.kind() == TableKind::Pseudo),
                None => true,
            })
            .map(|(_, relative_keys, schema)| CompletionItem {
                label: relative_keys.to_string(),
                kind: Some(CompletionItemKind::VARIABLE),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range: doc
                        .mapper
                        .range(entry_keys.all_text_range())
                        .unwrap()
                        .into_lsp(),
                    new_text: new_entry_snippet(&relative_keys, &schema, false),
                })),
                ..schema_annotated_item(&schema)
            })
            .collect(),
    )))
}

fn documentation(schema: &Value) -> Option<Documentation> {
    super::hover::schema_docs(schema).map(|value| {
        Documentation::MarkupContent(MarkupContent {
            kind: lsp_types::MarkupKind::Markdown,
            value,
        })
    })
}

/// The parts of a completion item that come from the schema rather than from
/// the position in the document.
///
/// Taplo sets both `tags` and the superseded `deprecated` because it never
/// reads the client's capabilities and so cannot tell which one the client
/// understands. Both are omitted from the wire when unset.
fn schema_annotated_item(schema: &Value) -> CompletionItem {
    let deprecated = super::hover::is_deprecated(schema);

    CompletionItem {
        documentation: documentation(schema),
        tags: deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
        deprecated: deprecated.then_some(true),
        ..Default::default()
    }
}

/// Renders a JSON value as a TOML literal, or nothing when it has no TOML
/// representation, as a null does.
fn toml_literal(value: &Value, single_quote: bool) -> Option<String> {
    let node: Node = serde_json::from_value(value.clone()).ok()?;
    Some(node.to_toml(true, single_quote))
}

fn add_value_completions(
    schema: &Value,
    range: Option<Range>,
    completions: &mut Vec<CompletionItem>,
    single_quote: bool,
) {
    let first = completions.len();

    add_value_completions_inner(schema, range, completions, single_quote);

    // A schema can offer the same literal as its default, one of its examples
    // and its type's placeholder. Only this call's items are compared, so two
    // branches of a `oneOf` still each contribute their own candidates.
    let mut seen = HashSet::new();
    let mut index = first;

    while index < completions.len() {
        if seen.insert(completions[index].label.clone()) {
            index += 1;
        } else {
            completions.remove(index);
        }
    }
}

fn add_value_completions_inner(
    schema: &Value,
    range: Option<Range>,
    completions: &mut Vec<CompletionItem>,
    single_quote: bool,
) {
    let ext = schema_ext_of(schema).unwrap_or_default();
    let ext_docs = ext.docs.unwrap_or_default();
    let enum_docs = ext_docs.enum_values.unwrap_or_default();

    let schema_docs = super::hover::schema_docs(schema);
    let deprecated = super::hover::is_deprecated(schema);

    if let Some(enum_values) = schema["enum"].as_array() {
        for (idx, val) in enum_values.iter().enumerate() {
            let node: Node = match serde_json::from_value(val.clone()) {
                Ok(v) => v,
                Err(err) => {
                    tracing::error!(error = %err, "failed to parse JSON");
                    continue;
                }
            };

            let toml_value = node.to_toml(true, single_quote);

            completions.push(CompletionItem {
                label: toml_value.clone(),
                sort_text: Some(format!("{idx}{toml_value}")),
                kind: Some(match node {
                    Node::Table(_) => CompletionItemKind::STRUCT,
                    _ => CompletionItemKind::VALUE,
                }),
                documentation: enum_docs
                    .get(idx)
                    .cloned()
                    .flatten()
                    .or_else(|| schema_docs.clone())
                    .map(|value| {
                        Documentation::MarkupContent(MarkupContent {
                            kind: lsp_types::MarkupKind::Markdown,
                            value,
                        })
                    }),
                tags: deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
                deprecated: deprecated.then_some(true),
                text_edit: range.map(|range| {
                    CompletionTextEdit::Edit(TextEdit {
                        range,
                        new_text: toml_value,
                    })
                }),
                ..Default::default()
            });
        }
        return;
    }

    if let Some(const_value) = schema.get("const") {
        if let Some(toml_value) = toml_literal(const_value, single_quote) {
            completions.push(CompletionItem {
                label: toml_value.clone(),
                kind: Some(CompletionItemKind::VALUE),
                documentation: ext_docs
                    .const_value
                    .clone()
                    .or_else(|| schema_docs.clone())
                    .map(|value| {
                        Documentation::MarkupContent(MarkupContent {
                            kind: lsp_types::MarkupKind::Markdown,
                            value,
                        })
                    }),
                tags: deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
                deprecated: deprecated.then_some(true),
                text_edit: range.map(|range| {
                    CompletionTextEdit::Edit(TextEdit {
                        range,
                        new_text: toml_value,
                    })
                }),
                ..Default::default()
            });
        }

        return;
    }

    if let Some(default_value) = schema.get("default") {
        if let Some(toml_value) = toml_literal(default_value, single_quote) {
            completions.push(CompletionItem {
                label: toml_value.clone(),
                detail: Some("default".into()),
                kind: Some(CompletionItemKind::VALUE),
                documentation: ext_docs
                    .default_value
                    .clone()
                    .or_else(|| schema_docs.clone())
                    .map(|value| {
                        Documentation::MarkupContent(MarkupContent {
                            kind: lsp_types::MarkupKind::Markdown,
                            value,
                        })
                    }),
                tags: deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
                deprecated: deprecated.then_some(true),
                text_edit: range.map(|range| {
                    CompletionTextEdit::Edit(TextEdit {
                        range,
                        new_text: toml_value,
                    })
                }),
                ..Default::default()
            });
        }
    }

    if let Some(examples) = schema["examples"].as_array() {
        for example in examples {
            let Some(toml_value) = toml_literal(example, single_quote) else {
                continue;
            };

            completions.push(CompletionItem {
                label: toml_value.clone(),
                detail: Some("example".into()),
                kind: Some(CompletionItemKind::VALUE),
                documentation: schema_docs.clone().map(|value| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: lsp_types::MarkupKind::Markdown,
                        value,
                    })
                }),
                tags: deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
                deprecated: deprecated.then_some(true),
                text_edit: range.map(|range| {
                    CompletionTextEdit::Edit(TextEdit {
                        range,
                        new_text: toml_value,
                    })
                }),
                ..Default::default()
            });
        }
    }

    let types = match schema["type"].clone() {
        Value::Null => Vec::from([Value::String("object".into())]),
        Value::String(s) => Vec::from([Value::String(s)]),
        Value::Array(tys) => tys,
        _ => Vec::new(),
    };

    for ty in types {
        if let Some(s) = ty.as_str() {
            match s {
                "string" => {
                    completions.push(CompletionItem {
                        label: r#""""#.into(),
                        kind: Some(CompletionItemKind::VALUE),
                        documentation: Some(Documentation::MarkupContent(MarkupContent {
                            kind: lsp_types::MarkupKind::Markdown,
                            value: schema_docs.clone().unwrap_or_else(|| "string".into()),
                        })),
                        tags: deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
                        deprecated: deprecated.then_some(true),
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        text_edit: range.map(|range| {
                            CompletionTextEdit::Edit(TextEdit {
                                range,
                                new_text: r#""$0""#.into(),
                            })
                        }),
                        ..Default::default()
                    });
                }
                "boolean" => {
                    completions.push(CompletionItem {
                        label: r"true".into(),
                        kind: Some(CompletionItemKind::VALUE),
                        documentation: Some(Documentation::MarkupContent(MarkupContent {
                            kind: lsp_types::MarkupKind::Markdown,
                            value: schema_docs.clone().unwrap_or_else(|| "true value".into()),
                        })),
                        tags: deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
                        deprecated: deprecated.then_some(true),
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        text_edit: range.map(|range| {
                            CompletionTextEdit::Edit(TextEdit {
                                range,
                                new_text: r"true$0".into(),
                            })
                        }),
                        ..Default::default()
                    });
                    completions.push(CompletionItem {
                        label: r"false".into(),
                        kind: Some(CompletionItemKind::VALUE),
                        documentation: Some(Documentation::MarkupContent(MarkupContent {
                            kind: lsp_types::MarkupKind::Markdown,
                            value: schema_docs.clone().unwrap_or_else(|| "false value".into()),
                        })),
                        tags: deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
                        deprecated: deprecated.then_some(true),
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        text_edit: range.map(|range| {
                            CompletionTextEdit::Edit(TextEdit {
                                range,
                                new_text: r"false$0".into(),
                            })
                        }),
                        ..Default::default()
                    });
                }
                "array" => {
                    completions.push(CompletionItem {
                        label: r"[]".into(),
                        kind: Some(CompletionItemKind::VALUE),
                        documentation: Some(Documentation::MarkupContent(MarkupContent {
                            kind: lsp_types::MarkupKind::Markdown,
                            value: schema_docs.clone().unwrap_or_else(|| "array".into()),
                        })),
                        tags: deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
                        deprecated: deprecated.then_some(true),
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        text_edit: range.map(|range| {
                            CompletionTextEdit::Edit(TextEdit {
                                range,
                                new_text: r"[$0]".into(),
                            })
                        }),
                        ..Default::default()
                    });
                }
                "object" => {
                    completions.push(CompletionItem {
                        label: r"{ }".into(),
                        kind: Some(CompletionItemKind::VALUE),
                        documentation: Some(Documentation::MarkupContent(MarkupContent {
                            kind: lsp_types::MarkupKind::Markdown,
                            value: schema_docs.clone().unwrap_or_else(|| "object".into()),
                        })),
                        tags: deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
                        deprecated: deprecated.then_some(true),
                        insert_text_format: Some(InsertTextFormat::SNIPPET),
                        text_edit: range.map(|range| {
                            CompletionTextEdit::Edit(TextEdit {
                                range,
                                new_text: r"{ $0 }".into(),
                            })
                        }),
                        ..Default::default()
                    });
                }
                _ => {}
            }
        }
    }
}

fn new_entry_snippet(keys: &Keys, schema: &Value, single_quote: bool) -> String {
    let value = default_value_snippet(schema, 0, single_quote);
    format!("{keys} = {value}")
}

fn default_value_snippet(
    schema: &Value,
    cursor_count: usize,
    single_quote: bool,
) -> Cow<'static, str> {
    if let Some(const_value) = schema.get("const") {
        if let Some(toml_value) = toml_literal(const_value, single_quote) {
            return format!("${{{cursor_count}:{toml_value}}}").into();
        }
    }

    if let Some(default_value) = schema.get("default") {
        if let Some(toml_value) = toml_literal(default_value, single_quote) {
            return format!("${{{cursor_count}:{toml_value}}}").into();
        }
    }

    if schema.get("enum").is_some() {
        return format!("${cursor_count}").into();
    }

    let mut init_keys = Vec::new();

    if let Some(ext) = schema_ext_of(schema) {
        if let Some(extra_init_keys) = ext.init_keys {
            init_keys.extend(extra_init_keys);
        }
    }

    if let Some(arr) = schema["required"].as_array() {
        init_keys.extend(
            arr.iter()
                .filter_map(|s| s.as_str().map(ToString::to_string)),
        );
    }

    init_keys.dedup();

    if !init_keys.is_empty() {
        let mut s = String::new();
        s += "{ ";

        for (i, init_key) in init_keys.iter().enumerate() {
            if i != 0 {
                s += ", ";
            }
            write!(
                s,
                "{init_key} = {}",
                default_value_snippet(
                    &schema["properties"][init_key],
                    cursor_count + 1,
                    single_quote
                )
            )
            .unwrap();
        }

        s += " }$0";

        return s.into();
    }

    empty_value_snippet(schema, cursor_count).into()
}

fn empty_value_snippet(schema: &Value, cursor_count: usize) -> String {
    if schema.is_schema_ref() {
        return format!("${cursor_count}");
    }

    match &schema["type"] {
        Value::Null => format!("{{ ${cursor_count} }}"),
        Value::String(s) => match s.as_str() {
            "object" => format!("{{ ${cursor_count} }}"),
            "array" => format!("[${cursor_count}]"),
            "string" => format!(r#""${cursor_count}""#),
            "boolean" => format!("${{{cursor_count}:false}}"),
            _ => format!("${cursor_count}"),
        },
        _ => format!("${cursor_count}"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::hover::tests::{complete_at, complete_at_line};
    use lsp_types::CompletionItemTag;
    use serde_json::json;

    fn labels(items: &[lsp_types::CompletionItem]) -> Vec<&str> {
        items.iter().map(|item| item.label.as_str()).collect()
    }

    #[tokio::test]
    async fn value_completion_offers_each_example() {
        let schema = json!({
            "type": "object",
            "properties": { "port": { "type": "integer", "default": 8080, "examples": [80, 443] } }
        });

        let items = complete_at(schema, "port = \n", 7).await;

        assert_eq!(labels(&items), ["8080", "80", "443"]);
        assert_eq!(items[0].detail.as_deref(), Some("default"));
        assert_eq!(items[1].detail.as_deref(), Some("example"));
        assert_eq!(items[2].detail.as_deref(), Some("example"));
    }

    #[tokio::test]
    async fn value_completion_never_repeats_a_label() {
        let schema = json!({
            "type": "object",
            "properties": {
                "on": { "type": "boolean", "default": true, "examples": [true, false] }
            }
        });

        let items = complete_at(schema, "on = \n", 5).await;

        assert_eq!(labels(&items), ["true", "false"]);
    }

    #[tokio::test]
    async fn value_completion_skips_examples_beside_an_enum() {
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": { "enum": ["fast", "slow"], "examples": ["other"] }
            }
        });

        let items = complete_at(schema, "mode = \n", 7).await;

        assert_eq!(labels(&items), ["\"fast\"", "\"slow\""]);
    }

    #[tokio::test]
    async fn value_completion_skips_examples_beside_a_const() {
        let schema = json!({
            "type": "object",
            "properties": { "mode": { "const": "only", "examples": ["other"] } }
        });

        let items = complete_at(schema, "mode = \n", 7).await;

        assert_eq!(labels(&items), ["\"only\""]);
    }

    #[tokio::test]
    async fn a_deprecated_key_is_tagged_in_entry_completion() {
        let schema = json!({
            "type": "object",
            "properties": {
                "old": { "type": "integer", "deprecated": true },
                "new": { "type": "integer" }
            }
        });

        let items = complete_at(schema, "\n", 0).await;

        let old = items.iter().find(|item| item.label == "old").unwrap();
        assert_eq!(
            old.tags.as_deref(),
            Some(&[CompletionItemTag::DEPRECATED][..])
        );
        assert_eq!(old.deprecated, Some(true));

        let new = items.iter().find(|item| item.label == "new").unwrap();
        assert_eq!(new.tags, None);
        assert_eq!(new.deprecated, None);
    }

    #[tokio::test]
    async fn a_deprecated_table_is_tagged_in_header_completion() {
        let schema = json!({
            "type": "object",
            "properties": {
                "old": { "type": "object", "deprecated": true, "properties": {} }
            }
        });

        // The header must be closed: `in_table_header` returns false without a
        // `]`, and the request falls through to the standalone-key path.
        let items = complete_at(schema, "[o]\n", 2).await;

        let old = items.iter().find(|item| item.label == "old").unwrap();
        assert_eq!(
            old.tags.as_deref(),
            Some(&[CompletionItemTag::DEPRECATED][..])
        );
        assert_eq!(old.deprecated, Some(true));
    }

    #[tokio::test]
    async fn a_deprecated_value_branch_tags_only_its_own_candidate() {
        let schema = json!({
            "type": "object",
            "properties": {
                "codec": {
                    "oneOf": [
                        { "const": "gzip", "deprecated": true },
                        { "const": "zstd" }
                    ]
                }
            }
        });

        let items = complete_at(schema, "codec = \n", 8).await;

        let gzip = items.iter().find(|item| item.label == "\"gzip\"").unwrap();
        assert_eq!(
            gzip.tags.as_deref(),
            Some(&[CompletionItemTag::DEPRECATED][..])
        );

        let zstd = items.iter().find(|item| item.label == "\"zstd\"").unwrap();
        assert_eq!(zstd.tags, None);
    }

    /// `Node`'s deserializer drops a null entry inside a table rather than
    /// rejecting the whole value, so a default holding one still renders.
    /// This records that behaviour, so rendering a default through a
    /// deserialize-or-skip helper cannot silently change it.
    #[tokio::test]
    async fn a_default_holding_a_null_renders_with_that_entry_dropped() {
        let schema = json!({
            "type": "object",
            "properties": { "port": { "type": "integer", "default": { "a": null } } }
        });

        let values = complete_at(schema.clone(), "port = \n", 7).await;
        assert_eq!(labels(&values), ["{  }"]);

        let keys = complete_at(schema, "\n", 0).await;
        assert_eq!(keys[0].insert_text.as_deref(), Some("port = ${0:{  }}"));
    }

    #[tokio::test]
    async fn key_completion_offers_only_the_selected_branch() {
        let schema = json!({
            "type": "object",
            "properties": { "kind": { "type": "string" } },
            "if": { "properties": { "kind": { "const": "docker" } }, "required": ["kind"] },
            "then": { "properties": { "image": { "type": "string" } } },
            "else": { "properties": { "binary": { "type": "string" } } }
        });

        let items = complete_at_line(schema, "kind = \"docker\"\ni\n", 1, 1).await;

        assert!(labels(&items).contains(&"image"));
        assert!(!labels(&items).contains(&"binary"));
    }
}
