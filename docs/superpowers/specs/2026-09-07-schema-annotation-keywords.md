# JSON Schema annotation keywords

**Status:** Draft
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

Probed through the real handlers at commit `50427f0`. Given

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

`CompletionItem` in `lsp-types` 0.93.2 carries both `deprecated: Option<bool>` (line 432, superseded) and `tags: Option<Vec<CompletionItemTag>>` (line 524), and `CompletionItemTag::DEPRECATED` exists (line 176). Every completion item Taplo builds ends in `..Default::default()`, so both are `None`. The five key-completion sites all set `documentation: documentation(&schema)` and nothing else annotation-shaped.

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

Each applicable schema produces a `HoverSections` value. Keywords contribute to it independently; rendering happens once at the end.

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

The handler then joins the non-empty per-schema renders. Because each schema can now end in a bullet list, and markdown merges two lists separated only by a blank line, the separator between schemas becomes a horizontal rule:

```rust
let content = schemas
    .iter()
    .map(|(_, schema)| key_hover_sections(schema, links_in_hover).render())
    .filter(|s| !s.is_empty())
    .join("\n\n---\n\n");
```

The `filter` is what removes the trailing separator probed above.

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

Banner order is link first, then the deprecation notice, which preserves the current position of the link.

The next feature set extends this by pushing onto `facts`: one line per constraint (``Minimum: `0` ``, ``Pattern: `^v\d+$` ``, `Unique items`). It adds contributors; it does not touch `render` or the handler.

`default` moves from a paragraph to a bullet. The string `default_value_markdown` returns is unchanged, so its unit test stands; only the `- ` prefix `render` adds is new.

### Documentation precedence

Three sources can supply a key's prose, and all three are read at four sites: the two hover branches, `documentation` in `completion.rs`, and `schema_docs` in `add_value_completions`.

**`x-taplo.docs.main` > `markdownDescription` > `description`.**

`x-taplo.docs.main` stays on top because it already outranks `description` at all four sites; demoting it would silently change the output of every schema that uses Taplo's own extension. `markdownDescription` outranks `description` because that is the VS Code convention this keyword exists to follow, and an author who writes both means the richer one for a markdown-rendering client. Taplo already declares `MarkupKind::Markdown` on every hover and every completion documentation it emits, so it is such a client.

Value hover keeps its existing last-resort fall-through to `title`; the new keyword slots in above `description`, not above `title`.

Non-string `markdownDescription` is ignored, exactly as a non-string `description` is today (`schema["description"].as_str()`).

### Annotations are read regardless of the declared draft

`deprecated` is 2019-09 and later; `readOnly` and `writeOnly` are draft 7 and later; `examples` is draft 6 and later. Taplo reads all four whatever the schema declares.

Rendering has no draft in hand and no cheap way to get one. `declared_draft` in `crates/taplo-common/src/schema/mod.rs` classifies the *root* schema's `$schema`; the schema under the cursor may have arrived through a `$ref` into another document with its own declaration, and `collect_schemas` carries no draft context at all. Threading one through `schemas_at_path` into both handlers is a traversal change to gate an annotation.

Gating one keyword and not its three neighbours would also be incoherent, and gating all four would mean a draft-7 schema that writes `deprecated: true` is ignored on a technicality the author plainly did not intend. VS Code's own JSON tooling honors these keywords without a draft check.

So `declared_draft` stays private to the `schema` module. Nothing in `taplo-common` changes.

Only the literal boolean `true` counts for `deprecated`, `readOnly` and `writeOnly`. `"deprecated": false` and `"deprecated": "use `bind` instead"` render nothing. Draft 2019-09 defines the keyword as a boolean; guessing at other shapes invents behavior no schema can rely on. Only a JSON array counts for `examples`.

`readOnly: true` and `writeOnly: true` together are contradictory, and both lines render. Hover reports what the schema says.

### `examples` as values

`examples` values render through the same path `default` uses: deserialize each into a `taplo::dom::Node` and call `to_toml(true, single_quote)`. An element that fails to deserialize is skipped rather than aborting the list, so one bad example does not cost the user the others.

In `add_value_completions` the examples are pushed immediately after the `default` candidate and before the type-shaped literals (`""`, `true`, `[]`, `{ }`), preserving the schema's own order. Two suppressions:

- `enum` and `const` return early from `add_value_completions` before `default` is reached. `examples` inherits that: a schema that enumerates its allowed values has already said everything, and examples would duplicate or contradict it.
- An example equal to the `default` is skipped, so the list carries no duplicate label.

Documentation on an example item is the schema's docs, matching what the `default` item falls back to.

### `deprecated` in completion

Key completion items get `tags: Some(vec![CompletionItemTag::DEPRECATED])`. The superseded `deprecated: Option<bool>` field is left `None`: the LSP specification replaced it with tags, and `lsp-types` 0.93.2 marks it accordingly.

All five key-completion sites share the annotation, so `documentation(&schema)` is replaced by a base item that carries both fields:

```rust
/// The parts of a key's completion item that come from the schema rather than
/// from the position in the document.
fn schema_annotated_item(schema: &Value) -> CompletionItem
```

Call sites change from `..Default::default()` to `..schema_annotated_item(&schema)` and drop their `documentation` line.

Value completion items are not tagged. `deprecated` sits on the key's schema, and by the time a user is choosing a value the key is already written; tagging every candidate for it repeats a signal they cannot act on there.

### Which hover branch

`hover.rs` has two branches. The `IDENT` branch describes a key and is where `default` already renders; it gets the full `HoverSections` treatment.

The primitive branch describes one written value, and its whole shape is a single docs string selected by matching the value against `enum`, `default` and `const`. It gets the documentation precedence change and nothing else. `Read-only` on a value the user just typed answers a question they did not ask; the key hover a keystroke away already says it.

### Testing at the handler

`hover` and `completion` take `Context<World<E>>`, whose fields are private and whose only constructor is `Server::create_context`. A `pub fn Context::detached(world: W)` in `crates/lsp-async-stub/src/lib.rs` closes that gap: it builds the same struct with an initialized `Inner` and a `futures::sink::drain()` writer, so messages written through it are discarded. Both handlers reply through their return value and never write to the socket, so a discarding writer costs the tests nothing.

With it, a test in `taplo-lsp` builds a `WorldState<NativeEnvironment>`, seeds the schema cache through `ws.schemas.cache().store(...)`, adds a glob `SchemaAssociation`, inserts a `DocumentState`, and calls the handler. No network, no server, no `initialize` handshake. Verified: the probe table under "Problem" was produced this way.

`taplo-lsp` has no `[dev-dependencies]`; the handlers are async, so `tokio = { workspace = true, features = ["macros", "rt"] }` is added, matching `taplo-common`.

## Behavior changes to accept

**Schemas are separated by a horizontal rule in key hover.** Multi-schema hover changes from `"a\n\nb"` to `"a\n\n---\n\nb"`. Only positions where more than one schema applies (`anyOf`, `oneOf`, a `$ref` beside siblings) are affected.

**A schema that contributes nothing no longer emits a separator.** The probed `"first branch\n\nsecond branch\n\n"` becomes `"first branch\n\n---\n\nsecond branch"`.

**`Default:` renders as a bullet.** `"desc\n\nDefault: `8080`"` becomes `"desc\n\n- Default: `8080`"`.

**A schema with `markdownDescription` and `description` changes what it shows.** The point of the change, and the only way an author gets what they wrote.

**Deprecated keys are struck through in completion lists.** How a client renders `CompletionItemTag::DEPRECATED` is the client's choice; VS Code strikes the label through.

**`Context::detached` is public API on `lsp-async-stub`.** The crate is published (version 0.7.0) and this widens its surface. It is a constructor for a type the crate already exports, it takes no new types, and it is what makes handler-level testing possible at all.

## Acceptance criteria

Every criterion is asserted through the real `hover` or `completion` handler unless it names a helper.

1. Key hover on a schema with `examples: [80, 443]` contains ``- Examples: `80`, `443` ``.
2. Key hover on a schema with `deprecated: true` contains `> **Deprecated**`, above the description. `deprecated: false` and a non-boolean `deprecated` contribute nothing.
3. Key hover on `readOnly: true` contains `- Read-only`; on `writeOnly: true`, `- Write-only`; a schema with both contains both.
4. Key hover on a schema carrying both `markdownDescription` and `description` shows the `markdownDescription`. A schema carrying `x-taplo.docs.main` as well shows the `x-taplo` text. A schema carrying only `description` is unchanged.
5. Value hover follows the same precedence, and still falls back to `title` when none of the three is present.
6. Completion in a value position offers one item per `examples` entry, after the `default` item, with no duplicate of the default. A schema with `enum` or `const` offers no example items.
7. A key completion item for a `deprecated: true` schema carries `tags: Some(vec![CompletionItemTag::DEPRECATED])`. One for a schema without it carries `tags: None`.
8. Key completion documentation follows the same precedence as criterion 4.
9. Key hover over an `anyOf` of two described branches contains exactly one `---` and no trailing separator.
10. `HoverSections::render` has unit tests for the empty value, docs-only, facts-only, and all-sections cases.
11. `cargo check --workspace --all-targets` and `cargo test --workspace` pass.

## Landing

Four commits, each building and passing `cargo test --workspace` on its own. This is one branch in a stack of five, so these are commits rather than separate pull requests; the branch opens one pull request.

1. `test(lsp): drive hover and completion from tests` — `Context::detached`, the `tokio` dev-dependency, and the shared test fixture, with a test asserting today's hover output. Nothing user-visible changes. This lands first because every later commit's test depends on it, and it is the only commit that touches `lsp-async-stub`.
2. `feat(lsp): structure hover content as sections` — `HoverSections`, `render`, the `---` separator and the empty-schema filter, with `default` moved onto the facts list. Carries criteria 9 and 10 and the three rendering changes under "Behavior changes to accept".
3. `feat(lsp): prefer markdownDescription over description` — the precedence change at all four sites. Criteria 4, 5 and 8.
4. `feat(lsp): render schema annotation keywords` — `examples`, `deprecated`, `readOnly`, `writeOnly` in hover; example value completions; the completion deprecation tag. Criteria 1, 2, 3, 6 and 7.

Commit 2 lands before 3 and 4 because both add contributors to the structure it introduces. Commit 3 is separable from 4 because it changes existing output rather than adding to it, and is the one most likely to be argued with.

## Reproducing the findings

The probe table under "Problem" is reproduced by adding `Context::detached` and the `tokio` dev-dependency, then writing a `#[tokio::test]` in `crates/taplo-lsp/src/handlers/hover.rs` that builds the world as described under "Testing at the handler" and panics with the handler's return value. Commit 1 of the landing plan leaves that fixture in the tree, so after it lands the probes are ordinary tests.

Verified at Taplo commit `50427f0`, against `lsp-types` 0.93.2.

## Open questions

1. **Is a horizontal rule the right separator between schemas in key hover?** It exists because two markdown bullet lists separated by a blank line merge into one. The alternatives are a bold heading per schema, an HTML `<hr>`, or leaving the blank line and accepting merged lists.
2. **Should `examples` be capped in hover?** The spec renders every element on one line. A schema with thirty examples produces a long line. A cap needs a number and an overflow rendering, both invented.
3. **Should the superseded `CompletionItem::deprecated` field be set alongside `tags`?** Setting both costs one line and covers clients older than LSP 3.15. Setting only `tags` follows the specification.
4. **Should `deprecated` also render on value hover?** The spec says no, on the grounds that the key hover is a keystroke away. A deprecated key whose value the user is reading is arguably worth interrupting for.
5. **Is `Context::detached` on a published crate acceptable, or should the test constructor be `#[doc(hidden)]` or feature-gated?** The alternative is driving `Server::handle_message` with a duplex stream, which needs the `initialize` handshake and a real writer.
6. **Should example values carry a distinguishing `detail` or `sort_text` in the completion list?** As specified they are indistinguishable from the `default` candidate except by position.
