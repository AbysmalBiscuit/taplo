# JSON Schema annotation keywords

**Status:** Accepted
**Tracking issue:** AbysmalBiscuit/taplo#1, "Annotation keywords" section

## Problem

JSON Schema's annotation keywords carry no constraints. Their only purpose is to reach a human, and the only place a Taplo user meets a schema is hover text and completion items. Taplo reads none of them.

`rg -n "examples|deprecated|readOnly|writeOnly|markdownDescription" crates/taplo-lsp/src/handlers/` returns nothing. Four keywords are dropped:

| Keyword | Since | What a user loses |
|---|---|---|
| `examples` | draft 6 | Sample values the author wrote, in hover and as completion candidates |
| `deprecated` | 2019-09 | No signal that a key is on its way out |
| `readOnly` / `writeOnly` | draft 7 | No signal that a key is not the user's to set |
| `markdownDescription` | VS Code convention | Rich prose falls back to the plain `description`, or to nothing |

Probed through the real handlers at commit `50427f0`, with the fixture described under "Reproducing the findings". Given

```json
{"type": "object", "properties": {"port": {
  "type": "integer",
  "description": "plain description",
  "markdownDescription": "**markdown** description",
  "default": 8080,
  "examples": [80, 443],
  "deprecated": true,
  "readOnly": true
}}}
```

and the document `port = 8080`:

| Probe | Result today |
|---|---|
| hover on the key `port` | `"plain description\n\nDefault: `8080`"` |
| hover on the value `8080` | `"plain description"` |
| completion in `port = ` | one item, label `8080`, from `default`; `tags: None` |
| completion on a partial key `p` | one item, label `port`, docs `"plain description"`, `tags: None` |

`markdownDescription` is silently outranked by `description`, and `examples`, `deprecated` and `readOnly` never appear.

### The popup has no structure to grow into

`hover.rs` builds key hover by mapping each applicable schema to a string and joining:

```rust
let content = schemas.iter().map(|(_, schema)| { /* ... */ }).join("\n\n");
```

Each per-schema string is itself `description`, then `\n\n`, then `Default: ...`, with an optional link glued on the front. Two problems follow, and both get worse with every keyword added.

**Empty sections still emit separators.** A schema that contributes nothing still takes part in the join. Probed on an `anyOf` of two described branches, hover returns `"first branch\n\nsecond branch\n\n"`: traversal yields three schemas, the `anyOf` container itself renders empty, and its separator survives.

**Flat prose does not survive more contributors.** Six items (`description`, `default`, `examples`, `deprecated`, `readOnly`, `writeOnly`) concatenated with blank lines read as an undifferentiated block, and the next feature set in the stack adds seventeen constraint keywords to the same popup. Whatever shape this feature set chooses, that one inherits.

### Completion has no deprecation channel

`CompletionItem` in `lsp-types` 0.93.2 carries both `deprecated: Option<bool>` (line 432, the pre-3.15 field) and `tags: Option<Vec<CompletionItemTag>>` (line 524), and `CompletionItemTag::DEPRECATED` exists (line 176). Every completion item Taplo builds ends in `..Default::default()`, so both are `None`. The six key-completion sites (`completion.rs` lines 123, 173, 217, 264, 324, 422) all set `documentation: documentation(&…)` and nothing else annotation-shaped. Lines 123 and 173 bind the schema as `s`, the rest as `schema`.

## Goals

- `examples` renders in hover and offers each value as a completion candidate.
- `deprecated` tags key completion items and shows in hover.
- `readOnly` and `writeOnly` show in hover.
- `markdownDescription` outranks `description` wherever `description` is read today.
- Hover gains a content structure that absorbs a keyword per line without rewriting, and stops emitting separators for schemas that say nothing.

## Non-goals

The constraint keywords (`minimum`, `pattern`, `minItems`, `format` and the rest of the tracking issue's "Constraints in hover" section). This spec builds the structure they land in and stops there.

Schema traversal. No keyword here changes which schema applies at a position, so `collect_schemas` in `crates/taplo-common/src/schema/mod.rs` is untouched.

Validation. `deprecated` produces no diagnostic; `readOnly` and `writeOnly` are not enforced against the document.

A `x-taplo.docs.examples` field. `ExtDocs` in `crates/taplo-common/src/schema/ext.rs` documents `const`, `default` and `enum` members individually; nothing in the tracking issue asks for the same on `examples`, and adding it is a schema-extension change with its own compatibility surface.

## Design

### Hover content is a section list, not a string

Each applicable schema produces a `HoverSections` value. Keywords contribute to it independently; rendering happens once at the end. The type lives at module level in `hover.rs`, not nested inside the `IDENT` branch, so the primitive branch can adopt it later.

```rust
/// One schema's hover text, assembled from independent contributors so that a
/// keyword with nothing to say adds no separator.
#[derive(Default)]
struct HoverSections {
    /// Whole-key notices, rendered above the documentation.
    banners: Vec<String>,
    /// Prose documentation for the key.
    docs: Option<String>,
    /// One line per keyword that carries a concrete value or constraint.
    facts: Vec<String>,
}
```

`render` emits, in order: each banner, the docs, then the facts as a single markdown bullet list. Blocks are joined with `\n\n` and empty blocks are dropped, so a schema with only facts renders as a bare list and a schema with nothing renders as the empty string.

The handler then joins the non-empty per-schema renders. Because each schema can now end in a bullet list, and markdown merges two lists separated only by a blank line into one loose list, the separator between schemas becomes a horizontal rule:

```rust
let content = schemas
    .iter()
    .map(|(_, schema)| key_hover_sections(schema, links_in_hover).render())
    .filter(|s| !s.is_empty())
    .join("\n\n---\n\n");
```

The `filter` is what removes the trailing separator probed above. The blank line before the `---` is load-bearing: `---` directly under a paragraph line is a setext heading underline, and the blank line is what makes it a thematic break. An HTML `<hr>` is not an option, since the VS Code client does not opt into HTML in markdown — `clientOpts` in `editors/vscode/src/client.ts` sets only `documentSelector` and `initializationOptions`, and `vscode-languageclient` passes raw HTML through only when `markdown.supportHtml` is set — so the tag would be stripped. A heading per schema needs a title, which `anyOf` branches rarely carry; the existing link code already falls back to `"..."` for that reason.

Mapping for this feature set:

| Contributor | Section | Rendering |
|---|---|---|
| `x-taplo.links.key` | banner | `[{title}]({link})` |
| `deprecated: true` | banner | `> **Deprecated**` |
| docs (see precedence below) | docs | verbatim |
| `default` | fact | ``Default: `8080` `` |
| `examples` | fact | ``Examples: `80`, `443` `` |
| `readOnly: true` | fact | `Read-only` |
| `writeOnly: true` | fact | `Write-only` |

Banner order is link first, then the deprecation notice, which preserves the current position of the link. Facts render in push order: values (`Default`, `Examples`) first, then access (`Read-only`, `Write-only`). Constraint facts are appended after these.

Every fact that quotes a value fences it through a shared helper rather than bare backticks:

```rust
/// Wraps a rendered value in a markdown code span, widening the fence past any
/// backtick run inside it.
fn code_span(text: &str) -> String
```

`default_value_markdown` uses single backticks today, which a value containing a backtick breaks. `examples` widens that exposure and the next feature set's `pattern` makes it routine.

The next feature set extends this by pushing onto `facts`: one line per constraint (``Minimum: `0` ``, ``Pattern: `^v\d+$` ``, `Unique items`). Paired keywords (`minimum`/`maximum`, `minLength`/`maxLength`, `minItems`/`maxItems`, `minProperties`/`maxProperties`, with `exclusiveMinimum`/`exclusiveMaximum` folding into the range line) are paired by the contributor pushing one combined string, which `Vec<String>` already allows. It adds contributors; it does not touch `render` or the handler.

`default` moves from a paragraph to a bullet. The string `default_value_markdown` returns is unchanged, so its unit test stands; only the `- ` prefix `render` adds is new.

### Documentation precedence

Three sources can supply a key's prose, and all three are read at four sites: `hover.rs:141` (key branch), `hover.rs:254` (primitive branch), `completion.rs:450` (`documentation`), and `completion.rs:472` (`schema_docs` in `add_value_completions`). Those are the only reads of `description` in the handlers; the `schema.description` reads in `associations.rs` are catalog metadata and out of scope.

**`x-taplo.docs.main` > `markdownDescription` > `description`.**

`x-taplo.docs.main` stays on top because it already outranks `description` at all four sites; demoting it would silently change the output of every schema that uses Taplo's own extension. `markdownDescription` outranks `description` because that is the VS Code convention this keyword exists to follow, and an author who writes both means the richer one for a markdown-rendering client. Taplo already declares `MarkupKind::Markdown` on every hover and every completion documentation it emits, so it is such a client.

Value hover keeps its existing last-resort fall-through to `title`; the new keyword slots in above `description`, not above `title`.

Non-string `markdownDescription` is ignored, exactly as a non-string `description` is today (`schema["description"].as_str()`).

`site/site/configuration/developing-schemas.md` documents `x-taplo.docs.main` as "If this is omitted, the description will be used". That sentence becomes wrong and is updated in the same commit.

### Annotations are read regardless of the declared draft

`deprecated` is 2019-09 and later; `readOnly` and `writeOnly` are draft 7 and later; `examples` is draft 6 and later. Taplo reads all four whatever the schema declares.

Rendering has no draft in hand and no cheap way to get one. `declared_draft` in `crates/taplo-common/src/schema/mod.rs` classifies the *root* schema's `$schema`; the schema under the cursor may have arrived through a `$ref` into another document with its own declaration, and `collect_schemas` carries no draft context at all. Threading one through `schemas_at_path` into both handlers is a traversal change to gate an annotation.

Gating one keyword and not its three neighbours would also be incoherent, and gating all four would mean a draft-7 schema that writes `deprecated: true` is ignored on a technicality the author plainly did not intend. VS Code's own JSON tooling honors these keywords without a draft check.

So `declared_draft` stays private to the `schema` module. Nothing in `taplo-common` changes.

Only the literal boolean `true` counts for `deprecated`, `readOnly` and `writeOnly`. A `false` or a non-boolean value renders nothing. Draft 2019-09 defines the keyword as a boolean; guessing at other shapes invents behavior no schema can rely on. Only a JSON array counts for `examples`.

`readOnly: true` and `writeOnly: true` together are contradictory, and both lines render. Hover reports what the schema says.

### `examples` as values

`examples` values render through the same path `default` uses: deserialize each into a `taplo::dom::Node` and call `to_toml(true, single_quote)`. `Node`'s deserializer rejects `Value::Null` outright (`visit_unit` in `crates/taplo/src/dom/serde.rs`), so an element that fails to deserialize is skipped rather than aborting the list, and one bad example does not cost the user the others.

The list is not capped. `default` renders a value of any size inline and `enum` offers every member as a completion; `examples` follows the same rule, and a long line wraps in the hover widget.

In `add_value_completions` the examples are pushed after the `default` candidate and before the type-shaped literals (`""`, `true`, `[]`, `{ }`). Response order is not display order: outside the `enum` branch no item sets `sort_text`, and a client falls back to the label when `sortText` is absent, so the list is shown label-sorted. Example items therefore carry `detail: "example"` and the `default` item gains `detail: "default"`; the label stays the value and the detail says where it came from. `sort_text` stays unset: the type literals carry none, and an ordering that puts `default` ahead of `""` would need a sort prefix collating below `"` (0x22), which is an invented rule.

Two suppressions:

- `enum` and `const` return early from `add_value_completions` before `default` is reached. `examples` inherits that: a schema that enumerates its allowed values has already said everything, and examples would duplicate or contradict it.
- An example whose rendered label equals the label of an item already pushed for the same schema is skipped, so neither the default nor a repeated example produces a duplicate.

Documentation on an example item is the schema's docs, matching what the `default` item falls back to.

The same deserialize-or-skip helper replaces the four `serde_json::from_value(...).unwrap()` calls on `const` and `default` at `completion.rs` 518, 550, 693 and 700. A `default` the `Node` deserializer rejects currently panics the completion handler, and leaving two `unwrap`s beside a new `continue` on the same values is incoherent.

### `deprecated` in completion

Key completion items get `tags: Some(vec![CompletionItemTag::DEPRECATED])` and `deprecated: Some(true)`. `tags` is the current protocol field; `deprecated` is the pre-3.15 field it superseded. Taplo sets both because it cannot pick one: `initialize` reads `initialization_options` and `workspace_folders` from `InitializeParams` and nothing else, so `completionItem.tagSupport` and `completionItem.deprecatedSupport` are never seen, and the server already sends `insert_text_format` and `text_edit` on the same unconditional basis. Both fields serialize only when set, so an item that is not deprecated is unchanged on the wire.

All six key-completion sites share the annotation, so `documentation(&…)` is replaced by a base item that carries all three fields:

```rust
/// The parts of a key's completion item that come from the schema rather than
/// from the position in the document.
fn schema_annotated_item(schema: &Value) -> CompletionItem
```

Call sites change from `..Default::default()` to `..schema_annotated_item(&…)` and drop their `documentation` line. After the change, `rg -n 'documentation\(&' crates/taplo-lsp/src/handlers/completion.rs` matches only inside the helper.

Value completion items are not tagged. `deprecated` sits on the key's schema, and by the time a user is choosing a value the key is already written; tagging every candidate for it repeats a signal they cannot act on there.

### Which hover branch

`hover.rs` has two branches. The `IDENT` branch describes a key and is where `default` already renders; it gets the full `HoverSections` treatment.

The primitive branch describes one written value, and its whole shape is a single docs string selected by matching the value against `enum`, `default` and `const`, with per-schema strings joined by a single newline. It gets the documentation precedence change and nothing else. A banner cannot be bolted onto that shape: `> **Deprecated**` followed by a newline and prose is a blockquote with a lazy continuation, so the docs would render inside the quote. Fixing that means giving the branch its own sections value, blank-line join and empty filter, which is the rewrite this spec confines to the key branch. Deprecation, `Read-only` and `Write-only` are properties of the key, and the key hover on the same line already says them. When the value branch grows sections it takes all of them at once, constraint keywords included. Its single-newline join is pre-existing and unchanged here.

### Testing at the handler

`hover` and `completion` take `Context<World<E>>`, whose fields are private and whose only constructor is the private `Server::create_context`, which needs an `&rpc::Request<D>`.

A `pub fn Context::detached(world: W) -> Context<W>` in `crates/lsp-async-stub/src/lib.rs` closes that gap: it builds the same struct with an initialized `Inner`, no handlers, a fresh cancel token, and a writer of `futures::sink::drain().sink_map_err(|never| match never {})`, since `Drain`'s error type is `Never` and `MessageWriter` requires `io::Error`. Messages written through it are discarded. Both handlers reply through their return value and never write to the socket, so a discarding writer costs the tests nothing.

The function is ordinary public API: it is a constructor for a type the crate already exports, takes no new types, and pulls no dependency. It is not `#[doc(hidden)]`, because `taplo-lsp` depends on it and hiding an API the workspace uses promises nothing. It is not feature-gated, because the workspace resolver is `2` (`Cargo.toml` line 5), so a dev-dependency's features are not activated for non-test builds anyway, and a feature that gates a dependency-free constructor isolates nothing.

With it, a test in `taplo-lsp` builds a `WorldState<NativeEnvironment>`, seeds the schema cache through `ws.schemas.cache().store(...)`, adds a glob `SchemaAssociation`, inserts a `DocumentState`, and calls the handler. No network, no server, no `initialize` handshake. `WorldState::new` seeds a default workspace at `root:///` that `by_document` falls back to, and `SchemaConfig::enabled` defaults true, so no configuration is needed. Verified: the probe table under "Problem" was produced this way.

`taplo-lsp` has no `[dev-dependencies]`; the handlers are async, so `tokio = { workspace = true, features = ["macros", "rt"] }` is added, matching `taplo-common`. That is the only dependency addition.

## Behavior changes to accept

**Schemas are separated by a horizontal rule in key hover.** Multi-schema hover changes from `"a\n\nb"` to `"a\n\n---\n\nb"`. Only positions where more than one schema applies (`anyOf`, `oneOf`, a `$ref` beside siblings) are affected.

**A schema that contributes nothing no longer emits a separator.** The probed `"first branch\n\nsecond branch\n\n"` becomes `"first branch\n\n---\n\nsecond branch"`.

**`Default:` renders as a bullet.** `"desc\n\nDefault: `8080`"` becomes `"desc\n\n- Default: `8080`"`.

**The `default` completion item gains `detail: "default"`.** Clients that show `detail` inline show one extra word beside the label.

**A schema with `markdownDescription` and `description` changes what it shows.** The point of the change, and the only way an author gets what they wrote.

**Deprecated keys are struck through in completion lists.** How a client renders `CompletionItemTag::DEPRECATED` is the client's choice; VS Code strikes the label through.

**`Context::detached` is public API on `lsp-async-stub`.** The crate is published (version 0.7.0) and this widens its surface by one constructor for a type it already exports. It is what makes handler-level testing possible without an `initialize` handshake.

## Acceptance criteria

Every criterion is asserted through the real `hover` or `completion` handler unless it names a helper.

1. Key hover on a schema with `examples: [80, 443]` contains ``- Examples: `80`, `443` ``.
2. Key hover on a schema with `deprecated: true` contains `> **Deprecated**`, above the description. A `false` or non-boolean `deprecated` contributes nothing.
3. Key hover on `readOnly: true` contains `- Read-only`; on `writeOnly: true`, `- Write-only`; a schema with both contains both.
4. Key hover on a schema carrying both `markdownDescription` and `description` shows the `markdownDescription`. A schema carrying `x-taplo.docs.main` as well shows the `x-taplo` text. A schema carrying only `description` is unchanged.
5. Value hover follows the same precedence, and still falls back to `title` when none of the three is present.
6. Completion in a value position offers one item per `examples` entry with `detail: Some("example")`, the `default` item carries `detail: Some("default")`, and no two items share a label. A schema with `enum` or `const` offers no example items.
7. A key completion item for a `deprecated: true` schema carries `tags: Some(vec![CompletionItemTag::DEPRECATED])` and `deprecated: Some(true)`; one for a schema without it carries `None` in both. Asserted at a table-header position and at an entry-key position, and `rg -n 'documentation\(&' crates/taplo-lsp/src/handlers/completion.rs` matches only inside `schema_annotated_item`.
8. Key completion documentation follows the same precedence as criterion 4.
9. Key hover over an `anyOf` of two described branches contains exactly one `---` and no trailing separator.
10. `HoverSections::render` has unit tests for the empty value, docs-only, facts-only, and all-sections cases, and `code_span` has one for a value containing a backtick.
11. The CI command set passes: `cargo test -p taplo`; `cargo test -p taplo-common --features schema,reqwest,rustls-tls`; `cargo check -p lsp-async-stub -p taplo-common -p taplo-lsp -p taplo` and the matching `cargo test`; `cargo check -p taplo-cli` and `cargo test -p taplo-cli`; and `cargo check --target wasm32-unknown-unknown` from `crates/taplo-wasm`. The wasm check matters because commit 1 touches `lsp-async-stub`; `Context::detached` uses only `futures`, which that build already compiles. `cargo check --workspace --all-targets` and `cargo test --workspace` also pass.

## Landing

Five commits, each building and passing the CI command set on its own. This is one branch in a stack of five, so these are commits rather than separate pull requests; the branch opens one pull request.

1. `test(lsp): drive hover and completion from tests` — `Context::detached`, the `tokio` dev-dependency, and the shared test fixture, with tests asserting today's hover and completion output. Nothing user-visible changes. This lands first because every later commit's test depends on it, and it is the only commit that touches `lsp-async-stub`. Its assertions on hover text are rewritten by commit 2, which is the point: they are the characterization that makes commit 2's diff legible, not a durable contract.
2. `feat(lsp): structure hover content as sections` — `HoverSections`, `render`, `code_span`, the `---` separator and the empty-schema filter, with `default` moved onto the facts list. Criteria 9 and 10, and the first three rendering changes under "Behavior changes to accept".
3. `feat(lsp): prefer markdownDescription over description` — the precedence change at all four sites, plus the `developing-schemas.md` correction. Criteria 4, 5 and 8.
4. `feat(lsp): render annotation keywords in hover` — `examples`, `deprecated`, `readOnly`, `writeOnly` as banner and fact contributors. Criteria 1, 2 and 3.
5. `feat(lsp): annotate completions from the schema` — example value candidates with `detail`, the shared deserialize-or-skip helper replacing the four `unwrap`s, and `schema_annotated_item` across the six key sites. Criteria 6 and 7.

Commit 2 lands before 3, 4 and 5 because they add contributors to the structure it introduces. Commit 3 is separable because it changes existing output rather than adding to it, and is the one most likely to be argued with. Commits 4 and 5 split hover from completion: they share no code beyond the keyword reads, and a reviewer can reject one while approving the other.

## Reproducing the findings

The probe table under "Problem" is reproduced by adding `Context::detached` and the `tokio` dev-dependency, then writing a `#[tokio::test]` in `crates/taplo-lsp/src/handlers/hover.rs` that builds the world as described under "Testing at the handler" and panics with the handler's return value. Commit 1 of the landing plan leaves that fixture in the tree, so after it lands the probes are ordinary tests.

Verified at Taplo commit `50427f0`, against `lsp-types` 0.93.2 and `futures` 0.3.30.
