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
use serde_json::{Number, Value};
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
    /// Rendered values: TOML literals, comparisons, regular expressions and
    /// format names. `HoverSections::render` fences each as a code span, so a
    /// contributor never writes a backtick itself.
    values: Vec<String>,
}

/// A subschema a schema names in a role that is not "the value here": a
/// prohibition, a rule for key names, a rule some element must satisfy.
struct NestedFacts {
    label: &'static str,
    facts: Vec<Fact>,
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
    /// One labelled block per subschema whose role is not "the value here",
    /// rendered beneath the flat facts.
    nested: Vec<NestedFacts>,
}

impl HoverSections {
    fn render(&self) -> String {
        let mut blocks: Vec<String> = self.banners.clone();

        blocks.extend(self.docs.clone());

        if !self.facts.is_empty() || !self.nested.is_empty() {
            let mut lines: Vec<String> =
                self.facts.iter().map(|fact| fact_line(fact, "")).collect();

            for nested in &self.nested {
                lines.push(format!("- {}", nested.label));
                lines.extend(nested.facts.iter().map(|fact| fact_line(fact, "  ")));
            }

            blocks.push(lines.join("\n"));
        }

        blocks.retain(|block| !block.is_empty());
        blocks.join("\n\n")
    }
}

/// One bullet for a fact, indented for a nested block.
fn fact_line(fact: &Fact, indent: &str) -> String {
    if fact.values.is_empty() {
        format!("{indent}- {}", fact.label)
    } else {
        format!(
            "{indent}- {}: {}",
            fact.label,
            fact.values.iter().map(|value| code_span(value)).join(", ")
        )
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

/// One end of a range: the bound and whether the schema excludes it.
struct Bound {
    value: Number,
    exclusive: bool,
}

impl Bound {
    /// The comparison a reader sees, given the operators for this side.
    fn render(&self, inclusive: &str, exclusive: &str) -> String {
        let operator = if self.exclusive { exclusive } else { inclusive };
        format!("{operator} {}", self.value)
    }
}

/// Renders the bounds on a quantity as one fact, so that a lower and an upper
/// bound share a line rather than taking one each.
///
/// A pair of inclusive bounds on the same value collapses to the bare value,
/// which is how a fixed size reads best. The collapse needs exactly one bound
/// per side, since a schema writing two bounds on one side has stated two
/// separate comparisons.
fn bounds_fact(label: &'static str, lower: Vec<Bound>, upper: Vec<Bound>) -> Option<Fact> {
    if lower.is_empty() && upper.is_empty() {
        return None;
    }

    if let ([low], [high]) = (lower.as_slice(), upper.as_slice()) {
        if !low.exclusive && !high.exclusive && low.value == high.value {
            return Some(Fact {
                label,
                values: vec![low.value.to_string()],
            });
        }
    }

    let values = lower
        .iter()
        .map(|bound| bound.render(">=", ">"))
        .chain(upper.iter().map(|bound| bound.render("<=", "<")))
        .collect();

    Some(Fact { label, values })
}

/// Reads one side of a numeric range.
///
/// Both spellings of an exclusive bound are honored, told apart by shape: a
/// number in the exclusive keyword is the bound itself, while a boolean beside
/// the inclusive keyword is draft 4's modifier on that bound. A schema may
/// write both, and both render.
fn numeric_bounds(schema: &Value, inclusive: &str, exclusive: &str) -> Vec<Bound> {
    let mut bounds = Vec::new();

    if let Some(value) = schema[inclusive].as_number() {
        bounds.push(Bound {
            value: value.clone(),
            exclusive: flag(schema, exclusive),
        });
    }

    if let Some(value) = schema[exclusive].as_number() {
        bounds.push(Bound {
            value: value.clone(),
            exclusive: true,
        });
    }

    bounds
}

/// The range a numeric schema admits.
fn range_fact(schema: &Value) -> Option<Fact> {
    bounds_fact(
        "Range",
        numeric_bounds(schema, "minimum", "exclusiveMinimum"),
        numeric_bounds(schema, "maximum", "exclusiveMaximum"),
    )
}

/// The step a numeric schema admits.
fn multiple_of_fact(schema: &Value) -> Option<Fact> {
    Some(Fact {
        label: "Multiple of",
        values: vec![schema["multipleOf"].as_number()?.to_string()],
    })
}

/// Reads a keyword that bounds a size or a count, which no draft spells as
/// exclusive.
///
/// The result is a `Vec` of at most one so that every caller of `bounds_fact`
/// has the same shape.
fn inclusive_bounds(schema: &Value, keyword: &str) -> Vec<Bound> {
    schema[keyword]
        .as_number()
        .map(|value| Bound {
            value: value.clone(),
            exclusive: false,
        })
        .into_iter()
        .collect()
}

/// A fact whose only value is a string the schema states verbatim, such as a
/// regular expression or a format name.
///
/// An empty string constrains nothing, and an empty code span is not a code
/// span, so it contributes no fact.
fn string_fact(schema: &Value, label: &'static str, keyword: &str) -> Option<Fact> {
    let value = schema[keyword].as_str().filter(|value| !value.is_empty())?;

    Some(Fact {
        label,
        values: vec![value.to_owned()],
    })
}

/// Whether a schema's declared `type` admits instances of `wanted`, and so
/// whether a keyword constraining `wanted` can ever fire.
///
/// Every constraint keyword is defined conditionally on the instance type, so
/// one written against a type the schema does not admit is vacuous and hover
/// leaves it out. A schema that declares no type, or declares one in a shape
/// the specification does not describe, admits everything: the author wrote
/// the keyword, and hover has nothing better to go on.
fn admits_type(schema: &Value, wanted: &str) -> bool {
    fn matches(declared: &str, wanted: &str) -> bool {
        declared == wanted || (wanted == "number" && declared == "integer")
    }

    match &schema["type"] {
        Value::String(declared) => matches(declared, wanted),
        Value::Array(declared) => {
            let mut named = declared.iter().filter_map(Value::as_str).peekable();
            named.peek().is_none() || named.any(|declared| matches(declared, wanted))
        }
        _ => true,
    }
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

/// The constraint keywords a schema states about its own value, in the order
/// hover lists them.
///
/// Each group is filtered by the type its keywords constrain, so a keyword
/// written against a type the schema does not admit is vacuous and left out.
fn constraint_facts(schema: &Value) -> Vec<Fact> {
    let mut facts = Vec::new();

    if admits_type(schema, "number") {
        facts.extend(range_fact(schema));
        facts.extend(multiple_of_fact(schema));
    }

    if admits_type(schema, "string") {
        facts.extend(bounds_fact(
            "Length",
            inclusive_bounds(schema, "minLength"),
            inclusive_bounds(schema, "maxLength"),
        ));
        facts.extend(string_fact(schema, "Pattern", "pattern"));
        facts.extend(string_fact(schema, "Format", "format"));
        facts.extend(string_fact(schema, "Media type", "contentMediaType"));
        facts.extend(string_fact(schema, "Encoding", "contentEncoding"));
    }

    if admits_type(schema, "array") {
        facts.extend(bounds_fact(
            "Items",
            inclusive_bounds(schema, "minItems"),
            inclusive_bounds(schema, "maxItems"),
        ));

        if flag(schema, "uniqueItems") {
            facts.push(Fact {
                label: "Unique items",
                values: Vec::new(),
            });
        }
    }

    if admits_type(schema, "object") {
        facts.extend(bounds_fact(
            "Properties",
            inclusive_bounds(schema, "minProperties"),
            inclusive_bounds(schema, "maxProperties"),
        ));
    }

    facts
}

/// Keywords a nested block can show: the ones `subschema_facts` renders, and
/// annotations that constrain nothing.
///
/// An allowlist rather than a list of what to refuse, because the two go stale
/// in opposite directions. `subschema_facts` is one level deep, and an
/// incomplete requirement is merely incomplete where an incomplete prohibition
/// is wrong: a `not` over `properties` and `required` together forbids one
/// value of one key, and a block naming only the key would read as forbidding
/// the key. A keyword nobody has heard of yet costs a missing block.
const RENDERABLE_SUBSCHEMA_KEYWORDS: &[&str] = &[
    "type",
    "const",
    "enum",
    "required",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "contentMediaType",
    "contentEncoding",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
    "title",
    "description",
    "markdownDescription",
    "$comment",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
    "$id",
    "$schema",
    "definitions",
    "$defs",
    "x-taplo",
];

/// The types a subschema admits.
fn type_fact(schema: &Value) -> Option<Fact> {
    let values: Vec<String> = match &schema["type"] {
        Value::String(name) => vec![name.clone()],
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    };

    (!values.is_empty()).then_some(Fact {
        label: "Type",
        values,
    })
}

/// The single value a subschema admits.
fn const_fact(schema: &Value) -> Option<Fact> {
    let node: Node = serde_json::from_value(schema.get("const")?.clone()).ok()?;

    Some(Fact {
        label: "Const",
        values: vec![node.to_toml(true, false)],
    })
}

/// The values a subschema admits, as TOML literals.
fn enum_fact(schema: &Value) -> Option<Fact> {
    let values: Vec<String> = schema["enum"]
        .as_array()?
        .iter()
        .filter_map(|value| serde_json::from_value::<Node>(value.clone()).ok())
        .map(|node| node.to_toml(true, false))
        .collect();

    (!values.is_empty()).then_some(Fact {
        label: "One of",
        values,
    })
}

/// The keys a subschema demands.
fn required_fact(schema: &Value) -> Option<Fact> {
    let values: Vec<String> = schema["required"]
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect();

    (!values.is_empty()).then_some(Fact {
        label: "Required",
        values,
    })
}

/// What a subschema states about itself, for a reader who has been told the
/// role it plays, or nothing when the block would be incomplete.
///
/// `type`, `const`, `enum` and `required` appear here and not among a schema's
/// own facts because a nested block has no other channel: at the top level the
/// written value shows its type, and `enum` and `const` reach the reader
/// through value completion and value hover.
fn subschema_facts(schema: &Value) -> Option<Vec<Fact>> {
    let keywords = schema.as_object()?;

    if !keywords
        .keys()
        .all(|keyword| RENDERABLE_SUBSCHEMA_KEYWORDS.contains(&keyword.as_str()))
    {
        return None;
    }

    let mut facts = Vec::new();

    facts.extend(type_fact(schema));
    facts.extend(const_fact(schema));
    facts.extend(enum_fact(schema));
    facts.extend(required_fact(schema));
    facts.extend(constraint_facts(schema));

    (!facts.is_empty()).then_some(facts)
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
    sections.facts.extend(constraint_facts(schema));

    if flag(schema, "readOnly") {
        sections.facts.push(Fact {
            label: "Read-only",
            values: Vec::new(),
        });
    }

    if flag(schema, "writeOnly") {
        sections.facts.push(Fact {
            label: "Write-only",
            values: Vec::new(),
        });
    }

    for (label, keyword) in [
        ("Must not match", "not"),
        ("Key names", "propertyNames"),
        ("Contains", "contains"),
    ] {
        if let Some(facts) = subschema_facts(&schema[keyword]) {
            sections.nested.push(NestedFacts { label, facts });
        }
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
        hover_at_line(schema, source, 0, character).await
    }

    /// Returns the markdown the hover handler produces at a position.
    ///
    /// A conditional schema needs the discriminator and the key it selects on
    /// different lines, which is why the line is a parameter.
    pub(crate) async fn hover_at_line(
        schema: serde_json::Value,
        source: &str,
        line: u32,
        character: u32,
    ) -> Option<String> {
        let (world, document_url) = world_with(schema, source).await;

        let hovered = hover(
            lsp_async_stub::Context::detached(world),
            Some(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: document_url },
                    position: LspPosition::new(line, character),
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

    /// Returns the completion items the handler produces at a position on line 0.
    pub(crate) async fn complete_at(
        schema: serde_json::Value,
        source: &str,
        character: u32,
    ) -> Vec<lsp_types::CompletionItem> {
        complete_at_line(schema, source, 0, character).await
    }

    /// Returns the completion items the handler produces at a position.
    pub(crate) async fn complete_at_line(
        schema: serde_json::Value,
        source: &str,
        line: u32,
        character: u32,
    ) -> Vec<lsp_types::CompletionItem> {
        let (world, document_url) = world_with(schema, source).await;

        let response = crate::handlers::completion(
            lsp_async_stub::Context::detached(world),
            Some(lsp_types::CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: document_url },
                    position: LspPosition::new(line, character),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .into(),
        )
        .await
        .unwrap();

        match response {
            Some(lsp_types::CompletionResponse::Array(items)) => items,
            other => panic!("expected an array of completion items, got {other:?}"),
        }
    }

    /// Returns the document links the handler produces for a whole document.
    ///
    /// `schema.links` is off by default, and the handler returns nothing
    /// without it.
    pub(crate) async fn links_at(
        schema: serde_json::Value,
        source: &str,
    ) -> Vec<lsp_types::DocumentLink> {
        let (world, document_url) = world_with(schema, source).await;

        {
            let mut workspaces = world.workspaces.write().await;
            workspaces
                .by_document_mut(&document_url)
                .config
                .schema
                .links = true;
        }

        crate::handlers::links(
            lsp_async_stub::Context::detached(world),
            Some(lsp_types::DocumentLinkParams {
                text_document: TextDocumentIdentifier { uri: document_url },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .into(),
        )
        .await
        .unwrap()
        .unwrap_or_default()
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
                Fact {
                    label: "Default",
                    values: vec!["1".into()],
                },
                Fact {
                    label: "Read-only",
                    values: Vec::new(),
                },
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
            facts: vec![Fact {
                label: "Default",
                values: vec!["1".into()],
            }],
            nested: Vec::new(),
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

    #[tokio::test]
    async fn key_hover_renders_a_two_sided_range() {
        assert_eq!(
            key_hover_for(json!({ "type": "integer", "minimum": 1, "maximum": 10 }))
                .await
                .unwrap(),
            "- Range: `>= 1`, `<= 10`"
        );
    }

    #[tokio::test]
    async fn key_hover_renders_a_one_sided_range() {
        assert_eq!(
            key_hover_for(json!({ "type": "integer", "minimum": 1 }))
                .await
                .unwrap(),
            "- Range: `>= 1`"
        );

        assert_eq!(
            key_hover_for(json!({ "type": "number", "maximum": 2.5 }))
                .await
                .unwrap(),
            "- Range: `<= 2.5`"
        );
    }

    #[tokio::test]
    async fn key_hover_renders_numeric_exclusive_bounds() {
        assert_eq!(
            key_hover_for(json!({
                "type": "integer",
                "exclusiveMinimum": 0,
                "exclusiveMaximum": 10
            }))
            .await
            .unwrap(),
            "- Range: `> 0`, `< 10`"
        );

        assert_eq!(
            key_hover_for(json!({
                "type": "integer",
                "exclusiveMinimum": 5,
                "exclusiveMaximum": 5
            }))
            .await
            .unwrap(),
            "- Range: `> 5`, `< 5`"
        );
    }

    #[tokio::test]
    async fn key_hover_honors_the_draft_4_boolean_exclusive_bounds() {
        assert_eq!(
            key_hover_for(json!({ "type": "integer", "minimum": 1, "exclusiveMinimum": true }))
                .await
                .unwrap(),
            "- Range: `> 1`"
        );

        assert_eq!(
            key_hover_for(json!({ "type": "integer", "maximum": 10, "exclusiveMaximum": true }))
                .await
                .unwrap(),
            "- Range: `< 10`"
        );

        assert_eq!(
            key_hover_for(json!({ "type": "integer", "minimum": 1, "exclusiveMinimum": false }))
                .await
                .unwrap(),
            "- Range: `>= 1`"
        );
    }

    #[tokio::test]
    async fn key_hover_ignores_a_boolean_exclusive_bound_with_no_neighbour() {
        assert_eq!(
            key_hover_for(json!({ "type": "integer", "exclusiveMinimum": true })).await,
            None
        );
    }

    #[tokio::test]
    async fn key_hover_renders_every_bound_written_on_one_side() {
        assert_eq!(
            key_hover_for(json!({ "type": "integer", "minimum": 5, "exclusiveMinimum": 0 }))
                .await
                .unwrap(),
            "- Range: `>= 5`, `> 0`"
        );
    }

    #[tokio::test]
    async fn key_hover_collapses_equal_inclusive_bounds() {
        assert_eq!(
            key_hover_for(json!({ "type": "integer", "minimum": 5, "maximum": 5 }))
                .await
                .unwrap(),
            "- Range: `5`"
        );
    }

    #[tokio::test]
    async fn key_hover_does_not_collapse_an_integer_against_a_float() {
        assert_eq!(
            key_hover_for(json!({ "type": "number", "minimum": 40, "maximum": 40.0 }))
                .await
                .unwrap(),
            "- Range: `>= 40`, `<= 40.0`"
        );
    }

    #[tokio::test]
    async fn key_hover_renders_multiple_of() {
        assert_eq!(
            key_hover_for(json!({ "type": "integer", "multipleOf": 5 }))
                .await
                .unwrap(),
            "- Multiple of: `5`"
        );
    }

    #[tokio::test]
    async fn key_hover_ignores_numeric_keywords_of_the_wrong_shape() {
        for property in [
            json!({ "type": "integer", "minimum": "1" }),
            json!({ "type": "integer", "multipleOf": "5" }),
            json!({ "type": "integer", "exclusiveMinimum": "0" }),
        ] {
            assert_eq!(key_hover_for(property).await, None);
        }
    }

    #[tokio::test]
    async fn key_hover_drops_numeric_keywords_the_declared_type_makes_dead() {
        for property in [
            json!({ "type": "string", "minimum": 1 }),
            json!({ "type": "string", "multipleOf": 5 }),
            json!({ "type": "array", "maximum": 10 }),
        ] {
            assert_eq!(key_hover_for(property).await, None);
        }
    }

    #[tokio::test]
    async fn key_hover_renders_numeric_constraints_for_an_untyped_schema() {
        assert_eq!(
            key_hover_for(json!({ "minimum": 1 })).await.unwrap(),
            "- Range: `>= 1`"
        );
    }

    #[tokio::test]
    async fn key_hover_renders_numeric_constraints_for_a_type_union() {
        assert_eq!(
            key_hover_for(json!({ "type": ["integer", "null"], "minimum": 1 }))
                .await
                .unwrap(),
            "- Range: `>= 1`"
        );
    }

    #[tokio::test]
    async fn key_hover_orders_constraints_between_values_and_access() {
        let content = key_hover_for(json!({
            "type": "integer",
            "description": "The port.",
            "default": 8080,
            "minimum": 1,
            "maximum": 65535,
            "readOnly": true
        }))
        .await
        .unwrap();

        assert_eq!(
            content,
            "The port.\n\n- Default: `8080`\n- Range: `>= 1`, `<= 65535`\n- Read-only"
        );
    }

    #[tokio::test]
    async fn key_hover_renders_a_string_length() {
        assert_eq!(
            key_hover_for(json!({ "type": "string", "minLength": 1, "maxLength": 128 }))
                .await
                .unwrap(),
            "- Length: `>= 1`, `<= 128`"
        );

        assert_eq!(
            key_hover_for(json!({ "type": "string", "maxLength": 128 }))
                .await
                .unwrap(),
            "- Length: `<= 128`"
        );
    }

    #[tokio::test]
    async fn key_hover_collapses_a_fixed_string_length() {
        assert_eq!(
            key_hover_for(json!({ "type": "string", "minLength": 40, "maxLength": 40 }))
                .await
                .unwrap(),
            "- Length: `40`"
        );
    }

    #[tokio::test]
    async fn key_hover_renders_a_pattern() {
        assert_eq!(
            key_hover_for(json!({ "type": "string", "pattern": r"^v\d+$" }))
                .await
                .unwrap(),
            r"- Pattern: `^v\d+$`"
        );
    }

    #[tokio::test]
    async fn key_hover_fences_a_pattern_containing_backticks() {
        assert_eq!(
            key_hover_for(json!({ "type": "string", "pattern": "a`b" }))
                .await
                .unwrap(),
            "- Pattern: ``a`b``"
        );

        assert_eq!(
            key_hover_for(json!({ "type": "string", "pattern": "`x" }))
                .await
                .unwrap(),
            "- Pattern: `` `x ``"
        );
    }

    #[tokio::test]
    async fn key_hover_renders_a_format_whether_or_not_taplo_enforces_it() {
        assert_eq!(
            key_hover_for(json!({ "type": "string", "format": "semver" }))
                .await
                .unwrap(),
            "- Format: `semver`"
        );

        assert_eq!(
            key_hover_for(json!({ "type": "string", "format": "uri-template" }))
                .await
                .unwrap(),
            "- Format: `uri-template`"
        );
    }

    #[tokio::test]
    async fn key_hover_renders_content_annotations() {
        assert_eq!(
            key_hover_for(json!({
                "type": "string",
                "contentMediaType": "application/json",
                "contentEncoding": "base64"
            }))
            .await
            .unwrap(),
            "- Media type: `application/json`\n- Encoding: `base64`"
        );
    }

    #[tokio::test]
    async fn key_hover_ignores_string_keywords_of_the_wrong_shape() {
        for property in [
            json!({ "type": "string", "pattern": 12 }),
            json!({ "type": "string", "format": 12 }),
            json!({ "type": "string", "pattern": "" }),
            json!({ "type": "string", "format": "" }),
            json!({ "type": "string", "contentMediaType": "" }),
            json!({ "type": "string", "contentEncoding": "" }),
            json!({ "type": "string", "minLength": "1" }),
        ] {
            assert_eq!(key_hover_for(property).await, None);
        }
    }

    #[tokio::test]
    async fn key_hover_drops_string_keywords_the_declared_type_makes_dead() {
        for property in [
            json!({ "type": "integer", "minLength": 5 }),
            json!({ "type": "integer", "pattern": "^a$" }),
            json!({ "type": "integer", "format": "email" }),
            json!({ "type": "integer", "contentEncoding": "base64" }),
        ] {
            assert_eq!(key_hover_for(property).await, None);
        }
    }

    #[tokio::test]
    async fn key_hover_renders_string_constraints_for_a_type_union() {
        assert_eq!(
            key_hover_for(json!({ "type": ["string", "null"], "minLength": 1 }))
                .await
                .unwrap(),
            "- Length: `>= 1`"
        );
    }

    #[tokio::test]
    async fn key_hover_renders_a_documented_string_key_as_five_bullets() {
        let content = key_hover_for(json!({
            "type": "string",
            "description": "The image tag to deploy.",
            "default": "latest",
            "examples": ["v1.2.3", "latest"],
            "minLength": 1,
            "maxLength": 128,
            "pattern": r"^[\w.-]+$",
            "format": "semver"
        }))
        .await
        .unwrap();

        assert_eq!(
            content,
            concat!(
                "The image tag to deploy.\n",
                "\n",
                "- Default: `\"latest\"`\n",
                "- Examples: `\"v1.2.3\"`, `\"latest\"`\n",
                "- Length: `>= 1`, `<= 128`\n",
                r"- Pattern: `^[\w.-]+$`",
                "\n",
                "- Format: `semver`"
            )
        );
    }

    #[tokio::test]
    async fn key_hover_renders_array_constraints() {
        assert_eq!(
            key_hover_for(json!({
                "type": "array",
                "minItems": 1,
                "maxItems": 3,
                "uniqueItems": true
            }))
            .await
            .unwrap(),
            "- Items: `>= 1`, `<= 3`\n- Unique items"
        );
    }

    #[tokio::test]
    async fn key_hover_ignores_a_unique_items_that_is_not_true() {
        for unique in [json!(false), json!("yes"), json!(1)] {
            assert_eq!(
                key_hover_for(json!({ "type": "array", "uniqueItems": unique })).await,
                None
            );
        }
    }

    #[tokio::test]
    async fn key_hover_renders_object_constraints() {
        assert_eq!(
            key_hover_for(json!({ "type": "object", "minProperties": 1, "maxProperties": 5 }))
                .await
                .unwrap(),
            "- Properties: `>= 1`, `<= 5`"
        );
    }

    #[tokio::test]
    async fn key_hover_drops_container_keywords_the_declared_type_makes_dead() {
        for property in [
            json!({ "type": "integer", "minItems": 5 }),
            json!({ "type": "integer", "uniqueItems": true }),
            json!({ "type": "string", "minProperties": 5 }),
            json!({ "type": "object", "minItems": 1 }),
        ] {
            assert_eq!(key_hover_for(property).await, None);
        }
    }

    #[tokio::test]
    async fn key_hover_renders_every_constraint_an_untyped_schema_writes() {
        let content = key_hover_for(json!({
            "minimum": 1,
            "multipleOf": 2,
            "minLength": 1,
            "pattern": "^a$",
            "format": "email",
            "contentMediaType": "text/plain",
            "contentEncoding": "base64",
            "minItems": 1,
            "uniqueItems": true,
            "minProperties": 1
        }))
        .await
        .unwrap();

        assert_eq!(
            content,
            concat!(
                "- Range: `>= 1`\n",
                "- Multiple of: `2`\n",
                "- Length: `>= 1`\n",
                "- Pattern: `^a$`\n",
                "- Format: `email`\n",
                "- Media type: `text/plain`\n",
                "- Encoding: `base64`\n",
                "- Items: `>= 1`\n",
                "- Unique items\n",
                "- Properties: `>= 1`"
            )
        );
    }

    #[tokio::test]
    async fn hover_reads_the_branch_the_document_selects() {
        let schema = json!({
            "type": "object",
            "properties": { "kind": { "type": "string" } },
            "if": { "properties": { "kind": { "const": "docker" } }, "required": ["kind"] },
            "then": { "properties": { "image": { "description": "the image to pull" } } },
            "else": { "properties": { "image": { "description": "the binary to run" } } }
        });

        let hovered = hover_at_line(schema, "kind = \"docker\"\nimage = \"nginx\"\n", 1, 1).await;

        assert_eq!(hovered.as_deref(), Some("the image to pull"));
    }

    #[tokio::test]
    async fn links_follow_the_branch_the_document_selects() {
        let schema = json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "object",
                    "properties": { "kind": { "type": "string" } },
                    "if": { "properties": { "kind": { "const": "docker" } }, "required": ["kind"] },
                    "then": {
                        "properties": {
                            "image": { "x-taplo": { "links": { "key": "https://example.com/image" } } }
                        }
                    },
                    "else": {
                        "properties": {
                            "image": { "x-taplo": { "links": { "key": "https://example.com/binary" } } }
                        }
                    }
                }
            }
        });

        let links = links_at(schema, "[server]\nkind = \"docker\"\nimage = \"nginx\"\n").await;

        let targets: Vec<String> = links
            .iter()
            .filter_map(|link| link.target.as_ref().map(ToString::to_string))
            .collect();

        assert_eq!(targets, ["https://example.com/image"]);
    }

    #[tokio::test]
    async fn renders_a_prohibition_as_a_labelled_block() {
        let schema = json!({
            "type": "object",
            "properties": {
                "tag": {
                    "type": "string",
                    "not": { "type": "string", "pattern": "^latest$" }
                }
            }
        });

        let hovered = hover_at(schema, "tag = \"v1\"\n", 1).await;

        assert_eq!(
            hovered.as_deref(),
            Some("- Must not match\n  - Type: `string`\n  - Pattern: `^latest$`")
        );
    }

    #[tokio::test]
    async fn renders_key_names_and_contains_as_labelled_blocks() {
        let schema = json!({
            "type": "object",
            "properties": {
                "env": {
                    "type": "object",
                    "minProperties": 1,
                    "propertyNames": { "pattern": "^[A-Z_]+$" }
                },
                "tags": {
                    "type": "array",
                    "contains": { "const": "release" }
                }
            }
        });

        assert_eq!(
            hover_at(schema.clone(), "env = {}\n", 1).await.as_deref(),
            Some("- Properties: `>= 1`\n- Key names\n  - Pattern: `^[A-Z_]+$`")
        );
        assert_eq!(
            hover_at(schema, "tags = []\n", 1).await.as_deref(),
            Some("- Contains\n  - Const: `\"release\"`")
        );
    }

    #[tokio::test]
    async fn a_subschema_with_nothing_to_show_produces_no_block() {
        let cases = [
            json!({ "not": {} }),
            json!({ "not": { "properties": { "a": { "const": 1 } }, "required": ["a"] } }),
            json!({ "not": { "dependentRequired": { "a": ["b"] } , "required": ["a"] } }),
        ];

        for case in cases {
            let mut property = case.clone();
            property["type"] = json!("string");

            let schema = json!({
                "type": "object",
                "properties": { "tag": property }
            });

            assert_eq!(
                hover_at(schema, "tag = \"v1\"\n", 1).await,
                None,
                "case {case}"
            );
        }
    }

    #[tokio::test]
    async fn a_prohibition_states_what_a_subschema_requires() {
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": {
                    "not": { "enum": ["debug", "trace"] }
                }
            }
        });

        let hovered = hover_at(schema, "mode = \"fast\"\n", 1).await;

        assert_eq!(
            hovered.as_deref(),
            Some("- Must not match\n  - One of: `\"debug\"`, `\"trace\"`")
        );
    }
}
