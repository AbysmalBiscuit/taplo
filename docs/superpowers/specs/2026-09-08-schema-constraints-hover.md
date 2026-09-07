# JSON Schema constraints in hover

**Status:** Draft
**Tracking issue:** AbysmalBiscuit/taplo#1, "Constraints in hover" section

## Problem

A schema's constraint keywords are the part a user most needs before typing a value: the allowed range, the length, the pattern, the format. Taplo reads none of them.

```
rg -n '"(minimum|maximum|exclusiveMinimum|exclusiveMaximum|multipleOf|minLength|maxLength|pattern|minItems|maxItems|uniqueItems|minProperties|maxProperties|format|contentMediaType|contentEncoding)"' crates/taplo-lsp/src/
```

returns nothing. Sixteen keywords validate and stay invisible. The user meets each one for the first time as a red squiggle under a value they already typed.

Two of the sixteen are worse than invisible: `contentMediaType` and `contentEncoding` are annotations rather than assertions from 2019-09 on, so under the drafts most schemas declare there is no squiggle either. Probed through `Schemas::validate` (see "Reproducing the findings"), a schema with `contentEncoding: "base64"` against the string `"!!!not base64!!!"`:

| Declared draft | Errors |
|---|---|
| `http://json-schema.org/draft-07/schema#` | 1 |
| `https://json-schema.org/draft/2019-09/schema` | 0 |
| `https://json-schema.org/draft/2020-12/schema` | 0 |

For those two keywords under a modern draft, hover is not a convenience. It is the only channel that exists.

### `format` does not assert uniformly

`create_validator` calls `should_validate_formats(true)` (`crates/taplo-common/src/schema/mod.rs:280`) so that `format` keeps asserting under 2019-09 and 2020-12, where the specification demotes it to an annotation. That flag turns the keyword on; it does not give `jsonschema` 0.17.1 a validator for every format under every draft. `src/keywords/format.rs:424-510` dispatches on `context.config.draft()`, and several formats have arms for draft 6, draft 7 and 2019-09 but none for 2020-12.

Probed the same way, one property per format, each against a value that format rejects:

| Declared draft | Formats that assert | Formats that stay silent |
|---|---|---|
| draft-07 | date-time, date, time, email, idn-email, hostname, idn-hostname, ipv4, ipv6, uri, uri-reference, iri, iri-reference, uri-template, json-pointer, relative-json-pointer, regex, semver, semver-requirement | duration, uuid, *unknown* |
| 2019-09 | all of the above plus duration and uuid | *unknown* |
| 2020-12 | date-time, date, time, email, idn-email, hostname, ipv4, ipv6, uri, regex, semver, semver-requirement | duration, idn-hostname, iri, iri-reference, json-pointer, relative-json-pointer, uri-reference, uri-template, uuid, *unknown* |

Nine built-in formats stop asserting under 2020-12, seven of which do assert under draft 7. An unknown format never asserts under any draft: `are_unknown_formats_ignored` defaults true.

The two formats `create_validator` registers itself, `semver` and `semver-requirement`, assert under every draft. `format::compile` consults `context.config.format(...)` before the built-in match, so a custom format never reaches the draft-gated arms. `custom_formats_still_assert_under_draft_2020_12` in `crates/taplo-common/src/schema/tests.rs` already asserts this.

So whether a given `format` bites depends on the format name and the declared draft together, and hover has neither the draft nor a reason to want it. This spec's answer is under "Hover states the schema, not the enforcement".

### A keyword written on the wrong type is dead

Each assertion keyword constrains exactly one JSON type, and the validator applies it only to instances of that type. `minLength` on a schema whose `type` is `integer` never fires. Probed, every one of these produces zero errors:

| Schema | Instance |
|---|---|
| `{"type": "integer", "minLength": 5}` | `3` |
| `{"type": "integer", "pattern": "^a$"}` | `3` |
| `{"type": "integer", "minItems": 5}` | `3` |
| `{"type": "integer", "format": "email"}` | `3` |
| `{"type": "integer", "uniqueItems": true}` | `3` |
| `{"type": "string", "minimum": 5}` | `"a"` |
| `{"type": "string", "minProperties": 5}` | `"a"` |

Such a keyword is a schema bug. Rendering it would state a constraint the document cannot violate.

### Sixteen keywords do not fit in sixteen bullets

The hover popup already carries, per applicable schema, a link banner, a deprecation banner, the documentation, `Default`, `Examples`, `Read-only` and `Write-only`, with schemas separated by `---`. Pushing one `Fact` per constraint keyword on top of that is the wall of text the tracking issue's last checklist item warns about. The shape has to come out of the design, not out of a cap applied afterwards.

## Goals

- Every constraint keyword in the tracking issue's "Constraints in hover" section reaches the reader through key hover.
- A key carrying several constraints reads as a short list, not a wall.
- A keyword that cannot bite is not rendered.
- Nothing outside `crates/taplo-lsp/src/handlers/hover.rs` changes.

## Non-goals

**Validation.** Every keyword here already validates, or is defined not to. Nothing in this spec changes a diagnostic, and nothing makes `contentEncoding` assert under 2019-09.

**Traversal.** No keyword here changes which schema applies at a position, so `collect_schemas` and `collect_child_schemas` in `crates/taplo-common/src/schema/mod.rs` are untouched. The applicator keywords that do change traversal are the tracking issue's next section.

**Completion.** A constraint narrows the values a key accepts; it does not enumerate them, which is what a completion item needs. `enum` and `const` already do that. Rendering `- Pattern: ...` as a completion candidate would offer the user a regular expression to type into their document.

**The value-hover branch.** Its whole shape is a single documentation string selected by matching the written value against `enum`, `default` and `const`, joined across schemas with a single newline. The previous feature set confined the sections rewrite to the key branch for that reason, and the key hover on the same line carries every constraint already.

**Draft-aware rendering.** Argued below.

**A `x-taplo` override for any constraint keyword.** Nothing in the tracking issue asks for one, and each would be a schema-extension change with its own compatibility surface.

## Design

### Constraints are `Fact`s, and `render` is unchanged

The previous feature set left `hover.rs` with the structure this feature set fills in:

```rust
struct Fact { label: &'static str, values: Vec<String> }
struct HoverSections { banners: Vec<String>, docs: Option<String>, facts: Vec<Fact> }
```

`render` emits banners, then docs, then the facts as one markdown bullet list, drops empty blocks and joins the rest with `\n\n`. It applies `code_span` to every value, so a `pattern` carrying backticks is fenced correctly without a contributor doing anything.

`render` does not change. The wall of text is prevented by producing fewer facts, not by restructuring the list, through three mechanisms:

1. **Paired bounds collapse onto one line.** Five keyword pairs become at most five bullets instead of ten, and a pair whose bounds are equal becomes one bare value.
2. **Facts are filtered by the declared type.** A keyword that cannot bite is not rendered, so a string key never shows a numeric range and an integer key never shows a pattern.
3. **Fact order is fixed**, so the popup reads the same way for every schema.

Grouping three bullets under a heading costs more lines than it saves, and the type filter is what keeps the count at three. A hard cap was rejected: it would hide whichever constraint happened to sort last, and the reader has no way to ask for the rest.

### Facts

| Fact | Keywords | Constrains |
|---|---|---|
| `Range` | `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum` | number, integer |
| `Multiple of` | `multipleOf` | number, integer |
| `Length` | `minLength`, `maxLength` | string |
| `Pattern` | `pattern` | string |
| `Format` | `format` | string |
| `Media type` | `contentMediaType` | string |
| `Encoding` | `contentEncoding` | string |
| `Items` | `minItems`, `maxItems` | array |
| `Unique items` | `uniqueItems` | array |
| `Properties` | `minProperties`, `maxProperties` | object |

Facts are pushed in that order, after `Default` and `Examples` and before `Read-only` and `Write-only`. Within the popup the constraints therefore sit between what the key is worth and how the key may be used, which is the order a reader asks the questions in. The type order — number, string, array, object — matters only for a schema that declares no type, and follows the tracking issue's own checklist order.

`Unique items` is the only constraint fact with no values, so it renders as a bare `- Unique items`; `render` already handles that case.

### Bounds

A bounded pair renders as one fact whose values are the comparisons that apply:

| Schema | Rendered |
|---|---|
| `minimum: 1`, `maximum: 10` | ``- Range: `>= 1`, `<= 10` `` |
| `minimum: 1` | ``- Range: `>= 1` `` |
| `exclusiveMinimum: 0`, `maximum: 10` | ``- Range: `> 0`, `<= 10` `` |
| `minLength: 40`, `maxLength: 40` | ``- Length: `40` `` |
| `maxItems: 3` | ``- Items: `<= 3` `` |

ASCII comparison operators rather than interval notation. `[1, 10]` is a TOML array literal, and a hover popup over a TOML document is the one place that collision is guaranteed to be read the wrong way. `>=` also carries its meaning one-sided, which interval notation does not: `[1,` is not a thing to write.

Both bounds inclusive and equal collapse to the bare value, because `Length: >= 40, <= 40` is a fixed-width string stated in the least readable way available. The rule is shared by all four bounded facts rather than special-cased for lengths; a `Range` of exactly one number is rare but reads the same way.

Two spellings of an exclusive bound are honored, distinguished by shape rather than by draft:

- A **number** in `exclusiveMinimum` or `exclusiveMaximum` is the draft-6 form: the bound itself.
- A **boolean** `true` in `exclusiveMinimum` beside a `minimum` is the draft-4 form: a modifier that makes `minimum` exclusive.

`declared_draft` classifies `Draft4` (`crates/taplo-common/src/schema/mod.rs:732`), so a draft-4 schema is one Taplo validates, and rendering `>= 1` for a schema that means `> 1` would be wrong rather than merely incomplete. The two forms cannot collide: draft 4 defines the keyword as a boolean and draft 6 onwards as a number, so the JSON type settles which was meant without consulting `$schema`.

A `minimum` that is not a number, or an `exclusiveMinimum` that is neither a number nor a boolean, contributes nothing. Bounds render through `serde_json::Number`'s own `Display`, not through `f64`, so a bound outside `f64`'s exact integer range is stated as written.

### The type filter

```rust
/// Whether a schema's declared `type` admits instances of `wanted`, and so
/// whether a keyword constraining `wanted` can ever fire.
///
/// A schema that declares no type, or declares one in a shape the
/// specification does not describe, admits everything: the author wrote the
/// keyword, and hover has nothing better to go on.
fn constrains_type(schema: &Value, wanted: &str) -> bool
```

`type` may be a string, an array of strings, or absent. An array matches when any member matches. `integer` counts as `number`, since the numeric keywords constrain both.

The filter only ever removes a keyword the validator has already decided not to apply, which is why it is safe to apply it without knowing the draft: no draft has ever made `minLength` assert against an integer.

A schema that arrives at hover through `allOf` composition may carry the constraint without the `type` that its sibling branch declares. That schema declares no type, so the filter admits everything and the constraint renders. The filter subtracts only where a type is present and contradicts the keyword.

### Hover states the schema, not the enforcement

`format`, `contentMediaType` and `contentEncoding` render as plain facts. Hover does not mark which of them Taplo enforces, and does not consult the declared draft.

Three reasons.

**Hover's subject is the schema.** ``- Format: `uri-template` `` says the schema declares that format, which is true under every draft. It is the same statement ``- Pattern: `^v\d+$` `` makes, and hover does not promise there either that Taplo's regular expression engine agrees with ECMA-262 on every construct. A label that promised enforcement would need a per-format, per-draft truth table to stay honest; a label that describes needs nothing.

**The information is what the reader wants either way.** `contentMediaType` is defined as an annotation. Its author wrote it to tell a reader what the string holds. That is exactly hover's job, and it is the only job the keyword has ever had.

**Getting a draft to the renderer costs more than the answer is worth.** `declared_draft` is private to `crates/taplo-common/src/schema/mod.rs`, and the previous feature set left it private deliberately: it classifies the *root* schema, while the schema under the cursor may have arrived through a `$ref` into a document with its own declaration, and `collect_schemas` carries no draft context. Reaching it from `taplo-lsp` needs `pub`, not `pub(crate)`, plus a new accessor on `Schemas` and a draft threaded through `schemas_at_path` into the handler — a traversal change to footnote an annotation. Nothing in `taplo-common` changes.

The formats that stay silent under 2020-12 are a `jsonschema` 0.17 gap, not a Taplo design decision. The place to close it is an upstream bump or a `with_format` registration in `create_validator`, both of which live in the tracking issue's validation work rather than here.

### Worked example

A string key carrying documentation, a default, examples and three constraints:

```json
{
  "type": "string",
  "description": "The image tag to deploy.",
  "default": "latest",
  "examples": ["v1.2.3", "latest"],
  "minLength": 1,
  "maxLength": 128,
  "pattern": "^[\\w.-]+$",
  "format": "semver"
}
```

renders as

```markdown
The image tag to deploy.

- Default: `"latest"`
- Examples: `"v1.2.3"`, `"latest"`
- Length: `>= 1`, `<= 128`
- Pattern: `^[\w.-]+$`
- Format: `semver`
```

Five bullets for eight keywords, and the same schema with `type` removed adds nothing, because no other keyword is present. The upper bound is a schema that declares no type and writes every keyword: fourteen bullets, which is a schema nobody has written.

### Shape of the code

Ten contributors, four shared helpers, all in `hover.rs`:

```rust
/// One end of a range: the bound and whether the schema excludes it.
struct Bound { value: Number, exclusive: bool }

/// Renders a pair of bounds as one fact: `>= 1`, `<= 10`, or the bare bound
/// when both are inclusive and equal, which is how a fixed size reads best.
fn bounds_fact(label: &'static str, lower: Option<Bound>, upper: Option<Bound>) -> Option<Fact>

/// Reads a keyword that bounds a size or a count, which no draft spells as
/// exclusive.
fn inclusive_bound(schema: &Value, keyword: &str) -> Option<Bound>

/// A fact whose only value is a string the schema states verbatim, such as a
/// regular expression or a format name.
fn string_fact(schema: &Value, label: &'static str, keyword: &str) -> Option<Fact>
```

`uniqueItems` reuses the existing `flag` helper, which counts only a literal `true`, matching `readOnly` and `writeOnly`. `key_hover_sections` gains four `if constrains_type(...)` blocks, so the filter is visible at the call site rather than repeated inside each contributor.

## Behavior changes to accept

**Key hover gains bullets for schemas that carry constraint keywords.** A key that previously produced no hover at all now produces one when its schema declares a constraint and nothing else. `{"type": "string", "minLength": 1}` goes from `None` to ``- Length: `>= 1` ``.

**A constraint keyword contradicting the declared type disappears from hover.** It produced no hover before this feature set either, so nothing is lost; the filter is what stops it from appearing.

**A `format` that Taplo does not enforce is still shown.** Deliberate, and argued above.

## Acceptance criteria

Every criterion is asserted through the real `hover` handler unless it names a helper. The fixture is `hover_at` / `key_hover_for` in `hover.rs`'s `mod tests`.

1. Key hover on `{"type": "integer", "minimum": 1, "maximum": 10}` is ``- Range: `>= 1`, `<= 10` ``.
2. `{"type": "integer", "exclusiveMinimum": 0, "exclusiveMaximum": 10}` is ``- Range: `> 0`, `< 10` ``; a one-sided `{"minimum": 1}` is ``- Range: `>= 1` ``.
3. `{"type": "integer", "minimum": 1, "exclusiveMinimum": true}` is ``- Range: `> 1` ``, and `{"type": "integer", "minimum": 1, "exclusiveMinimum": false}` is ``- Range: `>= 1` ``.
4. `{"type": "integer", "multipleOf": 5}` is ``- Multiple of: `5` ``.
5. `{"type": "string", "minLength": 1, "maxLength": 128}` is ``- Length: `>= 1`, `<= 128` ``, and `{"minLength": 40, "maxLength": 40}` collapses to ``- Length: `40` ``.
6. `{"type": "string", "pattern": "^v\\d+$"}` is ``- Pattern: `^v\d+$` ``, and a pattern containing a backtick is fenced wide enough to survive it.
7. `{"type": "string", "format": "semver"}` is ``- Format: `semver` ``, and `format: "uri-template"` renders identically rather than being marked or dropped.
8. `{"type": "string", "contentMediaType": "application/json", "contentEncoding": "base64"}` is ``- Media type: `application/json`\n- Encoding: `base64` ``.
9. `{"type": "array", "minItems": 1, "maxItems": 3, "uniqueItems": true}` is ``- Items: `>= 1`, `<= 3`\n- Unique items ``, and a `uniqueItems` that is `false` or not a boolean contributes nothing.
10. `{"type": "object", "minProperties": 1, "maxProperties": 5}` is ``- Properties: `>= 1`, `<= 5` ``.
11. Every keyword written on a schema whose declared `type` it cannot constrain renders nothing: the seven schema/instance pairs from the "dead keyword" table each produce `None` from key hover.
12. `{"type": ["string", "null"], "minLength": 1}` renders the length, and a schema with no `type` at all renders every constraint keyword it carries.
13. A non-numeric `minimum`, a non-string `pattern` and a non-string `format` each contribute nothing.
14. The full worked example above renders exactly the five bullets shown, in that order, under its documentation.
15. `cargo check --workspace --all-targets`, `cargo test --workspace` and `cargo check --target wasm32-unknown-unknown` from `crates/taplo-wasm` are clean, along with the CI command set in `.github/workflows/ci.yaml`: `cargo test -p taplo`; `cargo test -p taplo-common --features schema,reqwest,rustls-tls`; `cargo check`/`cargo test` for `-p lsp-async-stub -p taplo-common -p taplo-lsp -p taplo` and for `-p taplo-cli`; and `cargo run -- fmt --check` followed by a clean `git diff-index --quiet HEAD --`.

## Landing

Three commits on `feat/schema-constraints-hover`, each building and passing on its own. This is one branch in a stack of five, so these are commits rather than separate pull requests; the branch opens one pull request.

1. `feat(lsp): render numeric constraints in hover` — `Bound`, `bounds_fact`, `inclusive_bound`, `constrains_type`, and the `Range` and `Multiple of` facts. The shared helpers land with the first facts that need them, and the type filter is provable here: `{"type": "string", "minimum": 1}` renders nothing. Criteria 1 through 4, and the numeric rows of 11 through 13.
2. `feat(lsp): render string constraints in hover` — `string_fact`, and the `Length`, `Pattern`, `Format`, `Media type` and `Encoding` facts. Criteria 5 through 8 and 14.
3. `feat(lsp): render array and object constraints` — `Items`, `Unique items` and `Properties`. Criteria 9, 10, and the remaining rows of 11 and 12.

The split is by constrained type because that is the boundary a reviewer can reject one side of: the argument about `format` and the argument about the draft-4 `exclusiveMinimum` are in different commits and share no code beyond helpers that commit 1 justifies on its own.

## Reproducing the findings

The three probe tables under "Problem" were produced by appending a `#[tokio::test]` to `crates/taplo-common/src/schema/tests.rs` that reuses the `seeded` fixture already there, calls `schemas.validate(&url, &instance)` for each row, prints the error count and ends in `panic!` so the output survives. Run with `cargo test -p taplo-common --features schema,reqwest,rustls-tls <name> -- --nocapture`. The probe was reverted; nothing in `taplo-common` changes.

Verified at `5a49d25`, against `jsonschema` 0.17.1 and `serde_json` 1.0.113.

## Open questions

1. Is the type filter right, or should hover render a constraint the schema declares even when the declared type makes it dead — on the argument that a silent drop hides a schema bug from the schema's own author?
2. Should `format` be rendered plainly, or should hover distinguish the formats Taplo actually enforces, accepting the cost of threading a draft into the renderer?
3. Are ASCII comparison operators the right notation for a bound, against interval brackets, against `1 to 10` prose, and against Unicode `≥`/`≤`?
4. Is the equal-inclusive-bounds collapse worth the branch, and should it apply to `Range` as well as to the three count facts?
5. Is honoring draft 4's boolean `exclusiveMinimum` in scope, or is it a keyword shape no schema Taplo will meet still uses?
6. Is `render` really unchanged, or does a popup that can reach fourteen bullets need a grouping mechanism that this spec declines to build?
7. Is a three-commit split by constrained type the right shape, or should the shared helpers and the type filter land as their own first commit?
8. `Items` and `Properties` as labels for counts — does `- Items: >= 1` read as a count, or does it need to say `Item count`?
