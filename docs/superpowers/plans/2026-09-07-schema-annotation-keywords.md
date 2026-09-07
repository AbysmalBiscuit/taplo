# JSON Schema annotation keywords Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render the JSON Schema annotation keywords `examples`, `deprecated`, `readOnly`/`writeOnly` and `markdownDescription` in the Taplo language server's hover text and completion items.

**Architecture:** Hover text stops being a concatenated string and becomes a `HoverSections { banners, docs, facts }` value that each keyword contributes to independently, rendered once at the end. Completion items gain a shared `schema_annotated_item` base carrying documentation and deprecation tags. Nothing in `taplo-common` changes: no keyword here affects which schema applies at a position.

**Tech Stack:** Rust 2021, `lsp-types` 0.93.2, `serde_json`, `tokio` (test-only), the workspace-local `lsp-async-stub` and `taplo` crates.

**Spec:** `docs/superpowers/specs/2026-09-07-schema-annotation-keywords.md`

## Global Constraints

- Work only in `/home/lev/Git/lev/taplo-wt/schema-annotations` on branch `feat/schema-annotations`. Use absolute paths; never `cd` first; use `git -C <abs path>`. Never `git stash`.
- Use `rg`, not `grep`; `fd`, not `find`. `rg -r` means `--replace`, so line numbers are `rg -n`.
- Conventional Commits, imperative mood, lowercase after the colon, subject ≤50 characters. Body only where the change needs context.
- Comments are timeless: state what the code does and the non-obvious why. Never "this PR", "now we", "previously"; never reference a plan task or issue number.
- Only the literal boolean `true` counts for `deprecated`, `readOnly` and `writeOnly`. Only a JSON array counts for `examples`. Only a string counts for `markdownDescription`.
- Documentation precedence everywhere: `x-taplo.docs.main` > `markdownDescription` > `description`.
- After any edit to a `.toml` file, run `cargo run -p taplo-cli -- fmt` from the worktree root. CI runs `taplo fmt --check` over `**/*.toml` and then `git diff-index --quiet`.
- Every task ends with a commit. Do not push, do not open a pull request.

**Verification command set** (referred to below as "the full check"):

```
cargo check --workspace --all-targets
cargo test --workspace
cargo check --target wasm32-unknown-unknown --manifest-path crates/taplo-wasm/Cargo.toml
cargo run -p taplo-cli -- fmt --check
```

---

## File Structure

- `crates/lsp-async-stub/src/lib.rs` — gains `Context::detached`, the only change to this crate.
- `crates/taplo-lsp/Cargo.toml` — gains a `[dev-dependencies]` table with `tokio`.
- `crates/taplo-lsp/src/handlers/hover.rs` — gains `HoverSections`, `Fact`, `code_span`, the keyword contributors, and the test fixture module. This is where most of the work lives.
- `crates/taplo-lsp/src/handlers/completion.rs` — gains `schema_annotated_item`, `toml_literal`, example candidates and label deduplication.
- `site/site/configuration/developing-schemas.md` — one sentence corrected.

The test fixture lives in `hover.rs`'s `mod tests` and is reached from `completion.rs`'s tests through `super::hover::tests`. `hover.rs` is the larger consumer and the fixture is small; a third module for it would be a file whose only job is holding four helper functions.

---

### Task 1: A detached context constructor

**Files:**
- Modify: `crates/lsp-async-stub/src/lib.rs` (inside `impl<W: Clone> Context<W>`, after `world()` at line 183)

**Interfaces:**
- Produces: `pub fn Context::detached(world: W) -> Context<W>`, used by every later task's tests.

**Why this is its own commit:** `lsp-async-stub` is published at version 0.7.0. Adding a public constructor is a change to its API surface and gets a commit subject that says so.

- [ ] **Step 1: Write the failing test**

Append to `crates/lsp-async-stub/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::Context;

    #[test]
    fn a_detached_context_carries_its_world() {
        let context = Context::detached(String::from("world"));
        assert_eq!(context.world(), "world");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lsp-async-stub a_detached_context`
Expected: FAIL to compile, `no function or associated item named 'detached' found`.

- [ ] **Step 3: Write minimal implementation**

In `crates/lsp-async-stub/src/lib.rs`, inside `impl<W: Clone> Context<W>`, immediately after the `world` method:

```rust
    /// Builds a context that is not attached to a server.
    ///
    /// Messages written through it are discarded, so it serves handlers that
    /// read the world and reply through their return value.
    pub fn detached(world: W) -> Self {
        Context {
            inner: Arc::new(AsyncMutex::new(Inner {
                next_request_id: 0,
                initialized: true,
                shutting_down: false,
                handlers: HashMap::new(),
                tasks: HashMap::new(),
                requests: HashMap::new(),
            })),
            cancel_token: Cancellation::default().token(),
            last_req_id: None,
            rw: Arc::new(AsyncMutex::new(Box::new(
                futures::sink::drain().sink_map_err(|never| match never {}),
            ))),
            world,
            deferred: Default::default(),
        }
    }
```

`futures::sink::drain()` has `Error = Infallible` while `MessageWriter` requires `io::Error`, so the `sink_map_err` with an empty match is what bridges them. `SinkExt` is already imported at the top of the file.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lsp-async-stub a_detached_context`
Expected: PASS.

- [ ] **Step 5: Run the full check**

Expected: all four commands clean. The wasm target matters here: this crate compiles into `taplo-wasm`.

- [ ] **Step 6: Commit**

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-annotations add crates/lsp-async-stub/src/lib.rs
git -C /home/lev/Git/lev/taplo-wt/schema-annotations commit -m "feat(lsp-async-stub): add detached context constructor"
```

---

### Task 2: Hover content as sections

**Files:**
- Modify: `crates/taplo-lsp/Cargo.toml` (add `[dev-dependencies]`)
- Modify: `crates/taplo-lsp/src/handlers/hover.rs` (the `IDENT` branch at lines 131-162, `default_value_markdown` at 289-299, and `mod tests` at 319-345)

**Interfaces:**
- Consumes: `Context::detached` from Task 1.
- Produces, all in `hover.rs` at module level:
  - `struct Fact { label: &'static str, values: Vec<String> }`
  - `struct HoverSections { banners: Vec<String>, docs: Option<String>, facts: Vec<Fact> }` with `#[derive(Default)]` and `fn render(&self) -> String`
  - `fn code_span(text: &str) -> String`
  - `fn key_hover_sections(schema: &Value, links_in_hover: bool) -> HoverSections`
  - `fn default_fact(schema: &Value) -> Option<Fact>` replacing `default_value_markdown`
  - `pub(crate) mod tests` exposing `async fn world_with(schema: Value, source: &str) -> (Arc<WorldState<NativeEnvironment>>, Url)` and `async fn hover_at(schema: Value, source: &str, character: u32) -> Option<String>` for Task 5's completion tests.

- [ ] **Step 1: Add the dev-dependency**

Append to `crates/taplo-lsp/Cargo.toml`:

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
```

Then run `cargo run -p taplo-cli -- fmt` from the worktree root so the table matches `taplo.toml`'s `align_entries` and `reorder_keys` rules.

- [ ] **Step 2: Write the failing tests**

Replace the whole `mod tests` at the bottom of `crates/taplo-lsp/src/handlers/hover.rs` with:

```rust
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
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p taplo-lsp`
Expected: FAIL to compile — `default_fact`, `HoverSections`, `Fact` and `code_span` do not exist.

- [ ] **Step 4: Write the implementation**

In `crates/taplo-lsp/src/handlers/hover.rs`, replace `default_value_markdown` (lines 286-299) with the section types and contributors, at module level:

```rust
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

/// Collects everything hover shows for a key from one schema.
fn key_hover_sections(schema: &Value, links_in_hover: bool) -> HoverSections {
    let ext = schema_ext_of(schema).unwrap_or_default();
    let ext_docs = ext.docs.unwrap_or_default();
    let ext_links = ext.links.unwrap_or_default();

    let mut sections = HoverSections::default();

    if links_in_hover {
        if let Some(link) = &ext_links.key {
            let link_title = schema["title"].as_str().unwrap_or("...");
            sections.banners.push(format!("[{link_title}]({link})"));
        }
    }

    sections.docs = ext_docs
        .main
        .or_else(|| schema["description"].as_str().map(Into::into));

    sections.facts.extend(default_fact(schema));

    sections
}
```

Then replace the `IDENT` branch's content builder (the `let content = schemas.iter().map(...).join("\n\n");` block at lines 131-162) with:

```rust
            let content = schemas
                .iter()
                .map(|(_, schema)| key_hover_sections(schema, links_in_hover).render())
                .filter(|rendered| !rendered.is_empty())
                .join("\n\n---\n\n");
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p taplo-lsp`
Expected: PASS, all tests in the module.

- [ ] **Step 6: Run the full check**

Expected: all four commands clean.

- [ ] **Step 7: Commit**

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-annotations add crates/taplo-lsp/
git -C /home/lev/Git/lev/taplo-wt/schema-annotations commit -m "feat(lsp): structure hover content as sections"
```

---

### Task 3: markdownDescription precedence

**Files:**
- Modify: `crates/taplo-lsp/src/handlers/hover.rs` (`key_hover_sections` from Task 2; the primitive branch's `else if let Some(desc) = schema["description"]` at line ~254)
- Modify: `crates/taplo-lsp/src/handlers/completion.rs` (`documentation` at 438-458, `schema_docs` in `add_value_completions` at 470-472)
- Modify: `site/site/configuration/developing-schemas.md`

**Interfaces:**
- Consumes: `world_with` and `hover_at` from Task 2.
- Produces: `pub(crate) fn schema_docs(schema: &Value) -> Option<String>` in `crates/taplo-lsp/src/handlers/hover.rs`, used by `completion.rs` as `super::hover::schema_docs`.

Putting the helper in `hover.rs` keeps it beside the other schema-reading helpers. `completion.rs` already reaches into shared code through `taplo_common::schema::ext`.

- [ ] **Step 1: Write the failing tests**

Add to `hover.rs`'s `mod tests`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p taplo-lsp`
Expected: FAIL to compile — `schema_docs` does not exist.

- [ ] **Step 3: Write the implementation**

Add to `hover.rs` at module level:

```rust
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
```

In `key_hover_sections`, replace the `sections.docs = ...` assignment with `sections.docs = schema_docs(schema);` and drop the now-unused `ext_docs` binding (keep `ext_links`).

In the primitive branch, replace the trailing selection (lines ~252-260):

```rust
                    if let Some(docs) = ext_docs.main {
                        docs
                    } else if let Some(desc) = schema["description"].as_str() {
                        desc.to_string()
                    } else if let Some(title) = schema["title"].as_str() {
                        title.to_string()
                    } else {
                        String::new()
                    }
```

with:

```rust
                    schema_docs(schema)
                        .or_else(|| schema["title"].as_str().map(Into::into))
                        .unwrap_or_default()
```

In `completion.rs`, replace the body of `documentation` (lines 438-458) with:

```rust
fn documentation(schema: &Value) -> Option<Documentation> {
    super::hover::schema_docs(schema).map(|value| {
        Documentation::MarkupContent(MarkupContent {
            kind: lsp_types::MarkupKind::Markdown,
            value,
        })
    })
}
```

and replace the `schema_docs` binding in `add_value_completions` (lines 470-472) with:

```rust
    let schema_docs = super::hover::schema_docs(schema);
```

The local binding keeps its name, so the six `schema_docs.clone()` uses below it are unchanged. Remove the now-unused `ext_docs.main` read but keep `ext_docs` itself, which still supplies `enum_values`, `const_value` and `default_value`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p taplo-lsp`
Expected: PASS.

- [ ] **Step 5: Correct the schema authoring docs**

In `site/site/configuration/developing-schemas.md`, replace:

```
      // Main documentation for the schema, it is expected to be markdown.
      // If this is omitted, the description will be used.
```

with:

```
      // Main documentation for the schema, it is expected to be markdown.
      // If this is omitted, `markdownDescription` is used, then `description`.
```

- [ ] **Step 6: Run the full check**

Expected: all four commands clean.

- [ ] **Step 7: Commit**

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-annotations add crates/taplo-lsp/ site/
git -C /home/lev/Git/lev/taplo-wt/schema-annotations commit -m "feat(lsp): prefer markdownDescription over description"
```

---

### Task 4: Annotation keywords in hover

**Files:**
- Modify: `crates/taplo-lsp/src/handlers/hover.rs` (`key_hover_sections` and its tests)

**Interfaces:**
- Consumes: `Fact`, `HoverSections`, `key_hover_sections`, `hover_at` from Tasks 2 and 3.
- Produces: `pub(crate) fn is_deprecated(schema: &Value) -> bool` in `hover.rs`, used by Task 5 from `completion.rs`.

- [ ] **Step 1: Write the failing tests**

Add to `hover.rs`'s `mod tests`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p taplo-lsp`
Expected: FAIL — the assertions return `None` or text without the new lines, because no contributor reads the keywords yet.

- [ ] **Step 3: Write the implementation**

Add to `hover.rs` at module level:

```rust
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
```

In `key_hover_sections`, after the link banner and before `sections.docs`:

```rust
    if is_deprecated(schema) {
        sections.banners.push("> **Deprecated**".into());
    }
```

and replace the single `sections.facts.extend(default_fact(schema));` with:

```rust
    sections.facts.extend(default_fact(schema));
    sections.facts.extend(examples_fact(schema));

    if flag(schema, "readOnly") {
        sections.facts.push(Fact { label: "Read-only", values: Vec::new() });
    }

    if flag(schema, "writeOnly") {
        sections.facts.push(Fact { label: "Write-only", values: Vec::new() });
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p taplo-lsp`
Expected: PASS.

- [ ] **Step 5: Run the full check**

Expected: all four commands clean.

- [ ] **Step 6: Commit**

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-annotations add crates/taplo-lsp/
git -C /home/lev/Git/lev/taplo-wt/schema-annotations commit -m "feat(lsp): render annotation keywords in hover"
```

---

### Task 5: Annotated completions

**Files:**
- Modify: `crates/taplo-lsp/src/handlers/completion.rs` (the six key-completion sites at lines 120-131, 170-181, 214-221, 261-286, 321-328, 419-433; `add_value_completions` at 460-679; `default_value_snippet` at 686-752)
- Modify: `crates/taplo-lsp/src/handlers/hover.rs` (test fixture gains a completion driver)

**Interfaces:**
- Consumes: `is_deprecated` from Task 4; `world_with` from Task 2.
- Produces: `fn schema_annotated_item(schema: &Value) -> CompletionItem` and `fn toml_literal(value: &Value, single_quote: bool) -> Option<String>` in `completion.rs`.

- [ ] **Step 1: Write the failing tests**

Add the completion driver to `hover.rs`'s `mod tests`:

```rust
    /// Returns the completion items the handler produces at a position on line 0.
    pub(crate) async fn complete_at(
        schema: serde_json::Value,
        source: &str,
        character: u32,
    ) -> Vec<lsp_types::CompletionItem> {
        let (world, document_url) = world_with(schema, source).await;

        let response = crate::handlers::completion(
            lsp_async_stub::Context::detached(world),
            Some(lsp_types::CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: document_url },
                    position: LspPosition::new(0, character),
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
```

Then create `crates/taplo-lsp/src/handlers/completion.rs`'s test module by appending:

```rust
#[cfg(test)]
mod tests {
    use super::super::hover::tests::complete_at;
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
        assert_eq!(old.tags.as_deref(), Some(&[CompletionItemTag::DEPRECATED][..]));
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

        let items = complete_at(schema, "[\n", 1).await;

        let old = items.iter().find(|item| item.label == "old").unwrap();
        assert_eq!(old.tags.as_deref(), Some(&[CompletionItemTag::DEPRECATED][..]));
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
        assert_eq!(gzip.tags.as_deref(), Some(&[CompletionItemTag::DEPRECATED][..]));

        let zstd = items.iter().find(|item| item.label == "\"zstd\"").unwrap();
        assert_eq!(zstd.tags, None);
    }

    #[tokio::test]
    async fn a_default_that_is_not_a_toml_value_does_not_panic() {
        let schema = json!({
            "type": "object",
            "properties": { "port": { "type": "integer", "default": { "a": null } } }
        });

        let values = complete_at(schema.clone(), "port = \n", 7).await;
        assert!(labels(&values).iter().all(|label| *label != "{ a = }"));

        complete_at(schema, "\n", 0).await;
    }
}
```

Register the module import in `completion.rs`'s header if needed; `super::super::hover::tests` resolves because `handlers.rs` declares both modules.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p taplo-lsp`
Expected: FAIL — no `detail`, no tags, and duplicate labels.

- [ ] **Step 3: Write the implementation**

Add to `completion.rs`:

```rust
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
```

Add `CompletionItemTag` to the `lsp_types` import list at the top.

At each of the six key-completion sites, delete the `documentation: documentation(&…),` line and change `..Default::default()` to `..schema_annotated_item(&…)`, keeping the site's own binding name (`s` at lines 120-131 and 170-181, `schema` at the other four).

Deduplication is one step at the end rather than a guard at each push site, because the type-shaped literals carry snippet edits that a shared push helper would flatten. Rename the existing function to `add_value_completions_inner` — changing nothing inside it beyond the edits below — and add a wrapper that keeps the old name and signature:

```rust
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
```

Add `use std::collections::HashSet;` to the imports.

Inside `add_value_completions_inner`, bind the deprecation once, immediately after the existing `schema_docs` binding:

```rust
    let deprecated = super::hover::is_deprecated(schema);
```

and add these two fields to every `CompletionItem` the function constructs, the `enum` branch and the four type arms included:

```rust
        tags: deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
        deprecated: deprecated.then_some(true),
```

Rewrite the `const` block (lines 516-546) as:

```rust
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
```

The old block chose `kind` by whether the node was a table; `toml_literal` no longer returns the node, and every `const` a TOML document can hold renders as a value, so `CompletionItemKind::VALUE` is used throughout.

Rewrite the `default` block (lines 548-575) the same way with `detail: Some("default".into())` and `ext_docs.default_value`, then append the examples immediately after it:

```rust
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
```

`toml_literal` returning `None` covers the old `!is_null()` guards, so those go away with the `unwrap`s. The four type arms are left exactly as they are apart from the two new fields; the wrapper's deduplication is what stops them repeating a default or an example.

In `default_value_snippet`, replace the two `serde_json::from_value(...).unwrap()` calls (lines 691-703) with `toml_literal`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p taplo-lsp`
Expected: PASS.

- [ ] **Step 5: Verify no key-completion site was missed**

Run: `rg -n 'documentation\(&' /home/lev/Git/lev/taplo-wt/schema-annotations/crates/taplo-lsp/src/handlers/completion.rs`
Expected: exactly one match, inside `schema_annotated_item`.

- [ ] **Step 6: Run the full check**

Expected: all four commands clean.

- [ ] **Step 7: Commit**

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-annotations add crates/taplo-lsp/
git -C /home/lev/Git/lev/taplo-wt/schema-annotations commit -m "feat(lsp): annotate completions from the schema"
```

---

## Acceptance criteria coverage

| Spec criterion | Task |
|---|---|
| 1 examples in hover | 4 |
| 2 deprecated banner | 4 |
| 3 read-only / write-only | 4 |
| 4 documentation precedence in key hover | 3 |
| 5 value hover precedence and title fallback | 3 |
| 6 example candidates with `detail` | 5 |
| 7 no repeated labels | 5 |
| 8 deprecation tag on key completions | 5 |
| 9 deprecation tag on a value branch | 5 |
| 10 a null-bearing default does not panic | 5 |
| 11 key completion documentation precedence | 3 |
| 12 one `---`, no trailing separator | 2 |
| 13 `render` and `code_span` unit tests | 2 |
| 14 the CI command set | every task's full check |
