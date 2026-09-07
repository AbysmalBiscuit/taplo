use crate::{
    query::{lookup_keys, Query},
    world::World,
};
use itertools::Itertools;
use lsp_async_stub::{
    rpc::Error,
    util::{LspExt, Position},
    Context, Params,
};
use lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};
use serde_json::Value;
use taplo::{
    dom::{KeyOrIndex, Keys, Node},
    syntax::SyntaxKind::{
        self, BOOL, DATE, DATE_TIME_LOCAL, DATE_TIME_OFFSET, IDENT, INTEGER, INTEGER_BIN,
        INTEGER_HEX, INTEGER_OCT, MULTI_LINE_STRING, MULTI_LINE_STRING_LITERAL, STRING,
        STRING_LITERAL, TIME,
    },
};
use taplo_common::{environment::Environment, schema::ext::schema_ext_of};

#[tracing::instrument(skip_all)]
pub(crate) async fn hover<E: Environment>(
    context: Context<World<E>>,
    params: Params<HoverParams>,
) -> Result<Option<Hover>, Error> {
    let p = params.required()?;

    let document_uri = p.text_document_position_params.text_document.uri;

    let workspaces = context.workspaces.read().await;
    let ws = workspaces.by_document(&document_uri);
    let doc = match ws.document(&document_uri) {
        Ok(d) => d,
        Err(error) => {
            tracing::debug!(%error, "failed to get document from workspace");
            return Ok(None);
        }
    };

    let position = p.text_document_position_params.position;
    let Some(offset) = doc.mapper.offset(Position::from_lsp(position)) else {
        tracing::error!(?position, "document position not found");
        return Ok(None);
    };

    let query = Query::at(&doc.dom, offset);

    let position_info = match query.before.clone().and_then(|p| {
        if p.syntax.kind() == IDENT || is_primitive(p.syntax.kind()) {
            Some(p)
        } else {
            None
        }
    }) {
        Some(before) => before,
        None => match query.after.clone().and_then(|p| {
            if p.syntax.kind() == IDENT || is_primitive(p.syntax.kind()) {
                Some(p)
            } else {
                None
            }
        }) {
            Some(after) => after,
            None => return Ok(None),
        },
    };

    if let Some(schema_association) = ws.schemas.associations().association_for(&document_uri) {
        tracing::debug!(
            schema.url = %schema_association.url,
            schema.name = schema_association.meta["name"].as_str().unwrap_or(""),
            schema.source = schema_association.meta["source"].as_str().unwrap_or(""),
            "using schema"
        );

        let value = match serde_json::to_value(&doc.dom) {
            Ok(v) => v,
            Err(error) => {
                tracing::warn!(%error, "cannot turn DOM into JSON");
                return Ok(None);
            }
        };

        let Some((keys, _)) = &position_info.dom_node else {
            return Ok(None);
        };

        let links_in_hover = !ws.config.schema.links;

        let mut keys = keys.clone();

        if let Some(header_key) = query.header_key() {
            let key_idx = header_key
                .descendants_with_tokens()
                .filter(|t| t.kind() == SyntaxKind::IDENT)
                .position(|t| t.as_token().unwrap() == &position_info.syntax)
                .unwrap();

            keys = lookup_keys(
                doc.dom.clone(),
                &Keys::new(keys.into_iter().take(key_idx + 1)),
            );
        }

        let Some(node) = doc.dom.path(&keys) else {
            return Ok(None);
        };

        if position_info.syntax.kind() == SyntaxKind::IDENT {
            keys = lookup_keys(doc.dom.clone(), &keys);

            // We're interested in the array itself, not its item type.
            while let Some(KeyOrIndex::Index(_)) = keys.iter().last() {
                keys = keys.skip_right(1);
            }

            let schemas = match ws
                .schemas
                .schemas_at_path(&schema_association.url, &value, &keys)
                .await
            {
                Ok(s) => s,
                Err(error) => {
                    tracing::error!(?error, "schema resolution failed");
                    return Ok(None);
                }
            };

            let content = schemas
                .iter()
                .map(|(_, schema)| key_hover_sections(schema, links_in_hover).render())
                .filter(|rendered| !rendered.is_empty())
                .join("\n\n---\n\n");

            if content.is_empty() {
                return Ok(None);
            }

            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: content,
                }),
                range: Some(
                    doc.mapper
                        .range(position_info.syntax.text_range())
                        .unwrap()
                        .into_lsp(),
                ),
            }));
        } else if is_primitive(position_info.syntax.kind()) {
            let schemas = match ws
                .schemas
                .schemas_at_path(&schema_association.url, &value, &keys)
                .await
            {
                Ok(s) => s,
                Err(error) => {
                    tracing::error!(?error, "schema resolution failed");
                    return Ok(None);
                }
            };

            let value = match serde_json::to_value(node) {
                Ok(v) => v,
                Err(error) => {
                    tracing::warn!(%error, "failed to turn DOM into JSON");
                    Value::Null
                }
            };

            let content = schemas
                .iter()
                .map(|(_, schema)| {
                    let ext = schema_ext_of(schema).unwrap_or_default();
                    let ext_docs = ext.docs.unwrap_or_default();
                    let enum_docs = ext_docs.enum_values.unwrap_or_default();

                    let ext_links = ext.links.unwrap_or_default();
                    let enum_links = ext_links.enum_values.unwrap_or_default();

                    if !enum_docs.is_empty() {
                        if let Some(enum_values) = schema["enum"].as_array() {
                            for (idx, val) in enum_values.iter().enumerate() {
                                if val == &value {
                                    if let Some(enum_docs) = enum_docs.get(idx).cloned().flatten() {
                                        if links_in_hover {
                                            let link_title =
                                                schema["title"].as_str().unwrap_or("...");

                                            if let Some(enum_link) =
                                                enum_links.get(idx).and_then(Option::as_ref)
                                            {
                                                return format!(
                                                    "[{link_title}]({enum_link})\n\n{enum_docs}"
                                                );
                                            }
                                        }

                                        return enum_docs;
                                    }
                                }
                            }
                        }
                    }

                    if let (Some(docs), Some(default_value)) =
                        (ext_docs.default_value, schema.get("default"))
                    {
                        if &value == default_value {
                            return docs;
                        }
                    }

                    if let (Some(docs), Some(const_value)) =
                        (ext_docs.const_value, schema.get("const"))
                    {
                        if &value == const_value {
                            return docs;
                        }
                    }

                    schema_docs(schema)
                        .or_else(|| schema["title"].as_str().map(Into::into))
                        .unwrap_or_default()
                })
                .join("\n");

            if content.is_empty() {
                return Ok(None);
            }

            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: content,
                }),
                range: Some(
                    doc.mapper
                        .range(position_info.syntax.text_range())
                        .unwrap()
                        .into_lsp(),
                ),
            }));
        }
    }

    Ok(None)
}

/// A labelled hover line: `Default` with one value, `Examples` with several,
/// `Read-only` with none.
struct Fact {
    label: &'static str,
    /// Rendered TOML literals. `HoverSections::render` fences each as a code
    /// span, so a contributor never writes a backtick itself.
    values: Vec<String>,
}

/// One schema's hover text, assembled from independent contributors so that a
/// keyword with nothing to say adds no separator.
#[derive(Default)]
struct HoverSections {
    /// Whole-key notices, rendered above the documentation.
    banners: Vec<String>,
    /// Prose documentation for the key.
    docs: Option<String>,
    /// One line per keyword that carries a concrete value or constraint.
    facts: Vec<Fact>,
}

impl HoverSections {
    fn render(&self) -> String {
        let mut blocks: Vec<String> = self.banners.clone();

        blocks.extend(self.docs.clone());

        if !self.facts.is_empty() {
            blocks.push(
                self.facts
                    .iter()
                    .map(|fact| {
                        if fact.values.is_empty() {
                            format!("- {}", fact.label)
                        } else {
                            format!(
                                "- {}: {}",
                                fact.label,
                                fact.values.iter().map(|v| code_span(v)).join(", ")
                            )
                        }
                    })
                    .join("\n"),
            );
        }

        blocks.retain(|block| !block.is_empty());
        blocks.join("\n\n")
    }
}

/// Wraps a value in a markdown code span wide enough to contain it.
///
/// A span's fence must be longer than any backtick run inside it, and a value
/// that begins or ends with a backtick needs a space on each side so the fence
/// is not absorbed into the content.
fn code_span(text: &str) -> String {
    let longest_run = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();

    let fence = "`".repeat(longest_run + 1);

    if text.starts_with('`') || text.ends_with('`') {
        format!("{fence} {text} {fence}")
    } else {
        format!("{fence}{text}{fence}")
    }
}

/// Renders a schema's `default` as a TOML literal, so that hovering a key shows
/// the value the tool falls back to. `x-taplo.docs.defaultValue` documents the
/// same keyword in prose and is rendered separately.
fn default_fact(schema: &Value) -> Option<Fact> {
    let default = schema.get("default")?;

    if default.is_null() {
        return None;
    }

    let node: Node = serde_json::from_value(default.clone()).ok()?;

    Some(Fact {
        label: "Default",
        values: vec![node.to_toml(true, false)],
    })
}

/// Reads a boolean annotation, which counts only when written as `true`.
///
/// The keyword is defined as a boolean, so any other shape is a schema the
/// specification does not describe and Taplo declines to guess at.
fn flag(schema: &Value, keyword: &str) -> bool {
    schema[keyword].as_bool().unwrap_or(false)
}

pub(crate) fn is_deprecated(schema: &Value) -> bool {
    flag(schema, "deprecated")
}

/// Renders a schema's `examples` as TOML literals, dropping any that is not a
/// TOML value so that one bad entry does not cost the reader the others.
fn examples_fact(schema: &Value) -> Option<Fact> {
    let values: Vec<String> = schema["examples"]
        .as_array()?
        .iter()
        .filter_map(|example| serde_json::from_value::<Node>(example.clone()).ok())
        .map(|node| node.to_toml(true, false))
        .collect();

    if values.is_empty() {
        return None;
    }

    Some(Fact {
        label: "Examples",
        values,
    })
}

/// The prose a schema offers for a key.
///
/// `x-taplo.docs.main` is Taplo's own override and outranks both standard
/// keywords; `markdownDescription` is the VS Code convention for rich text and
/// outranks the plain `description`. Every hover and completion documentation
/// Taplo emits is declared as markdown, so the richer text is always usable.
pub(crate) fn schema_docs(schema: &Value) -> Option<String> {
    let ext_docs = schema_ext_of(schema)
        .unwrap_or_default()
        .docs
        .unwrap_or_default();

    ext_docs
        .main
        .or_else(|| schema["markdownDescription"].as_str().map(Into::into))
        .or_else(|| schema["description"].as_str().map(Into::into))
}

/// Collects everything hover shows for a key from one schema.
fn key_hover_sections(schema: &Value, links_in_hover: bool) -> HoverSections {
    let ext = schema_ext_of(schema).unwrap_or_default();
    let ext_links = ext.links.unwrap_or_default();

    let mut sections = HoverSections::default();

    if links_in_hover {
        if let Some(link) = &ext_links.key {
            let link_title = schema["title"].as_str().unwrap_or("...");
            sections.banners.push(format!("[{link_title}]({link})"));
        }
    }

    if is_deprecated(schema) {
        sections.banners.push("> **Deprecated**".into());
    }

    sections.docs = schema_docs(schema);

    sections.facts.extend(default_fact(schema));
    sections.facts.extend(examples_fact(schema));

    if flag(schema, "readOnly") {
        sections.facts.push(Fact { label: "Read-only", values: Vec::new() });
    }

    if flag(schema, "writeOnly") {
        sections.facts.push(Fact { label: "Write-only", values: Vec::new() });
    }

    sections
}

fn is_primitive(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        BOOL | DATE
            | DATE_TIME_LOCAL
            | DATE_TIME_OFFSET
            | TIME
            | STRING
            | MULTI_LINE_STRING
            | STRING_LITERAL
            | MULTI_LINE_STRING_LITERAL
            | INTEGER
            | INTEGER_HEX
            | INTEGER_OCT
            | INTEGER_BIN
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lsp_async_stub::util::Mapper;
    use lsp_types::{
        Position as LspPosition, TextDocumentIdentifier, TextDocumentPositionParams, Url,
    };
    use serde_json::json;
    use std::sync::Arc;
    use taplo_common::{
        environment::native::NativeEnvironment,
        schema::associations::{priority, source, AssociationRule, SchemaAssociation},
    };

    use crate::world::{DocumentState, WorldState};

    /// Builds a world holding one document and one schema associated with it.
    ///
    /// `Cache::store` reports an error when no disk cache path is set but has
    /// already populated the in-memory cache, which is all a test needs.
    /// `NativeEnvironment::new` requires an active tokio runtime, so every
    /// caller must be a `#[tokio::test]`.
    pub(crate) async fn world_with(
        schema: serde_json::Value,
        source: &str,
    ) -> (Arc<WorldState<NativeEnvironment>>, Url) {
        let world = Arc::new(WorldState::new(NativeEnvironment::new()));
        let document_url: Url = "root:///test.toml".parse().unwrap();
        let schema_url: Url = "file:///taplo-test/schema.json".parse().unwrap();

        {
            let mut workspaces = world.workspaces.write().await;
            let ws = workspaces.by_document_mut(&document_url);

            drop(
                ws.schemas
                    .cache()
                    .store(schema_url.clone(), Arc::new(schema))
                    .await,
            );

            ws.schemas.associations().add(
                AssociationRule::glob("**/*.toml").unwrap(),
                SchemaAssociation {
                    url: schema_url,
                    meta: json!({ "source": source::MANUAL }),
                    priority: priority::MAX,
                },
            );

            let parse = taplo::parser::parse(source);
            let mapper = Mapper::new_utf16(source, false);
            let dom = parse.clone().into_dom();
            ws.documents
                .insert(document_url.clone(), DocumentState { parse, dom, mapper });
        }

        (world, document_url)
    }

    /// Returns the markdown the hover handler produces at a position on line 0.
    pub(crate) async fn hover_at(
        schema: serde_json::Value,
        source: &str,
        character: u32,
    ) -> Option<String> {
        let (world, document_url) = world_with(schema, source).await;

        let hovered = hover(
            lsp_async_stub::Context::detached(world),
            Some(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: document_url },
                    position: LspPosition::new(0, character),
                },
                work_done_progress_params: Default::default(),
            })
            .into(),
        )
        .await
        .unwrap()?;

        match hovered.contents {
            HoverContents::Markup(markup) => Some(markup.value),
            other => panic!("expected markup hover contents, got {other:?}"),
        }
    }

    fn described(description: &str) -> serde_json::Value {
        json!({ "type": "integer", "description": description })
    }

    #[test]
    fn renders_defaults_as_toml_literals() {
        let cases = [
            (json!(8080), "8080"),
            (json!("info"), "\"info\""),
            (json!(false), "false"),
            (json!([1, 2]), "[ 1, 2 ]"),
            (json!({ "level": 1 }), "{ level = 1 }"),
        ];

        for (default, expected) in cases {
            let schema = json!({ "default": default });
            let fact = default_fact(&schema).expect("no default fact");
            assert_eq!(fact.label, "Default");
            assert_eq!(fact.values, [expected]);
        }
    }

    #[test]
    fn skips_absent_and_null_defaults() {
        assert!(default_fact(&json!({})).is_none());
        assert!(default_fact(&json!({ "default": null })).is_none());
    }

    #[test]
    fn renders_nothing_for_empty_sections() {
        assert_eq!(HoverSections::default().render(), "");
    }

    #[test]
    fn renders_docs_alone_without_a_list() {
        let sections = HoverSections {
            docs: Some("prose".into()),
            ..Default::default()
        };

        assert_eq!(sections.render(), "prose");
    }

    #[test]
    fn renders_facts_alone_as_a_bullet_list() {
        let sections = HoverSections {
            facts: vec![
                Fact { label: "Default", values: vec!["1".into()] },
                Fact { label: "Read-only", values: Vec::new() },
            ],
            ..Default::default()
        };

        assert_eq!(sections.render(), "- Default: `1`\n- Read-only");
    }

    #[test]
    fn renders_every_section_separated_by_blank_lines() {
        let sections = HoverSections {
            banners: vec!["> **Deprecated**".into()],
            docs: Some("prose".into()),
            facts: vec![Fact { label: "Default", values: vec!["1".into()] }],
        };

        assert_eq!(
            sections.render(),
            "> **Deprecated**\n\nprose\n\n- Default: `1`"
        );
    }

    #[test]
    fn code_spans_widen_past_backticks_in_the_value() {
        assert_eq!(code_span("plain"), "`plain`");
        assert_eq!(code_span("a ` b"), "``a ` b``");
        assert_eq!(code_span("a ``` b"), "````a ``` b````");
    }

    #[test]
    fn code_spans_pad_values_that_start_or_end_with_a_backtick() {
        assert_eq!(code_span("`x"), "`` `x ``");
        assert_eq!(code_span("x`"), "`` x` ``");
    }

    #[tokio::test]
    async fn key_hover_renders_the_default_as_a_bullet() {
        let schema = json!({
            "type": "object",
            "properties": {
                "port": { "type": "integer", "description": "The port.", "default": 8080 }
            }
        });

        assert_eq!(
            hover_at(schema, "port = 8080\n", 1).await.as_deref(),
            Some("The port.\n\n- Default: `8080`")
        );
    }

    #[tokio::test]
    async fn key_hover_separates_schemas_with_a_rule() {
        let schema = json!({
            "type": "object",
            "properties": {
                "port": {
                    "anyOf": [described("first branch"), described("second branch")]
                }
            }
        });

        let content = hover_at(schema, "port = 8080\n", 1).await.unwrap();

        assert_eq!(content, "first branch\n\n---\n\nsecond branch");
        assert_eq!(content.matches("---").count(), 1);
    }

    #[test]
    fn documentation_precedence_prefers_taplo_then_markdown() {
        let all_three = json!({
            "x-taplo": { "docs": { "main": "taplo" } },
            "markdownDescription": "markdown",
            "description": "plain"
        });
        assert_eq!(schema_docs(&all_three).as_deref(), Some("taplo"));

        let two = json!({ "markdownDescription": "markdown", "description": "plain" });
        assert_eq!(schema_docs(&two).as_deref(), Some("markdown"));

        let one = json!({ "description": "plain" });
        assert_eq!(schema_docs(&one).as_deref(), Some("plain"));

        assert_eq!(schema_docs(&json!({})), None);
    }

    #[test]
    fn a_non_string_markdown_description_is_ignored() {
        let schema = json!({ "markdownDescription": 12, "description": "plain" });
        assert_eq!(schema_docs(&schema).as_deref(), Some("plain"));
    }

    #[tokio::test]
    async fn key_hover_prefers_the_markdown_description() {
        let schema = json!({
            "type": "object",
            "properties": {
                "port": {
                    "type": "integer",
                    "markdownDescription": "**rich**",
                    "description": "plain"
                }
            }
        });

        assert_eq!(
            hover_at(schema, "port = 8080\n", 1).await.as_deref(),
            Some("**rich**")
        );
    }

    #[tokio::test]
    async fn value_hover_prefers_the_markdown_description() {
        let schema = json!({
            "type": "object",
            "properties": {
                "port": {
                    "type": "integer",
                    "markdownDescription": "**rich**",
                    "description": "plain"
                }
            }
        });

        assert_eq!(
            hover_at(schema, "port = 8080\n", 8).await.as_deref(),
            Some("**rich**")
        );
    }

    #[tokio::test]
    async fn value_hover_still_falls_back_to_the_title() {
        let schema = json!({
            "type": "object",
            "properties": { "port": { "type": "integer", "title": "Port" } }
        });

        assert_eq!(
            hover_at(schema, "port = 8080\n", 8).await.as_deref(),
            Some("Port")
        );
    }

    /// Wraps a property schema in an object schema and hovers its key.
    async fn key_hover_for(property: serde_json::Value) -> Option<String> {
        let schema = json!({ "type": "object", "properties": { "port": property } });
        hover_at(schema, "port = 8080\n", 1).await
    }

    #[tokio::test]
    async fn key_hover_lists_examples() {
        let content = key_hover_for(json!({ "type": "integer", "examples": [80, 443] }))
            .await
            .unwrap();

        assert_eq!(content, "- Examples: `80`, `443`");
    }

    #[tokio::test]
    async fn key_hover_skips_examples_that_are_not_toml_values() {
        let content = key_hover_for(json!({ "type": "integer", "examples": [80, null] }))
            .await
            .unwrap();

        assert_eq!(content, "- Examples: `80`");
    }

    #[tokio::test]
    async fn key_hover_ignores_a_non_array_examples() {
        assert_eq!(
            key_hover_for(json!({ "type": "integer", "examples": 80 })).await,
            None
        );
    }

    #[tokio::test]
    async fn key_hover_banners_a_deprecated_key_above_the_docs() {
        let content = key_hover_for(json!({
            "type": "integer",
            "description": "The port.",
            "deprecated": true
        }))
        .await
        .unwrap();

        assert_eq!(content, "> **Deprecated**\n\nThe port.");
    }

    #[tokio::test]
    async fn key_hover_ignores_a_deprecated_that_is_not_true() {
        for deprecated in [json!(false), json!("yes"), json!(1)] {
            assert_eq!(
                key_hover_for(json!({ "type": "integer", "deprecated": deprecated })).await,
                None
            );
        }
    }

    #[tokio::test]
    async fn key_hover_notes_read_only_and_write_only() {
        assert_eq!(
            key_hover_for(json!({ "type": "integer", "readOnly": true }))
                .await
                .unwrap(),
            "- Read-only"
        );

        assert_eq!(
            key_hover_for(json!({ "type": "integer", "writeOnly": true }))
                .await
                .unwrap(),
            "- Write-only"
        );

        assert_eq!(
            key_hover_for(json!({ "type": "integer", "readOnly": true, "writeOnly": true }))
                .await
                .unwrap(),
            "- Read-only\n- Write-only"
        );
    }

    #[tokio::test]
    async fn key_hover_orders_values_before_access() {
        let content = key_hover_for(json!({
            "type": "integer",
            "description": "The port.",
            "default": 8080,
            "examples": [80],
            "readOnly": true,
            "deprecated": true
        }))
        .await
        .unwrap();

        assert_eq!(
            content,
            "> **Deprecated**\n\nThe port.\n\n- Default: `8080`\n- Examples: `80`\n- Read-only"
        );
    }
}
