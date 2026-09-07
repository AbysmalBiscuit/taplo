# JSON Schema constraints in hover

**Status:** Accepted
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

Such a keyword is a schema bug. Every assertion keyword in this spec is defined conditionally on the instance type, so the keyword is vacuous by the specification rather than by any choice of Taplo's: no instance of the declared type can violate it, under any draft or any validator. Rendering it would state a constraint that binds nothing the schema admits.

### Sixteen keywords do not fit in sixteen bullets

The hover popup already carries, per applicable schema, a link banner, a deprecation banner, the documentation, `Default`, `Examples`, `Read-only` and `Write-only`, with schemas separated by `---`. Pushing one `Fact` per constraint keyword on top of that is the wall of text the tracking issue's last checklist item warns about. The shape has to come out of the design, not out of a cap applied afterwards.

## Goals

- Every constraint keyword in the tracking issue's "Constraints in hover" section reaches the reader through key hover, wherever traversal reaches the schema that carries it.
- A key carrying several constraints reads as a short list, not a wall.
- A keyword that cannot bite is not rendered.
- Nothing outside `crates/taplo-lsp/src/handlers/hover.rs` changes.

## Non-goals

**Validation.** Every keyword here already validates, or is defined not to. Nothing in this spec changes a diagnostic, and nothing makes `contentEncoding` assert under 2019-09.

**Traversal.** No keyword here changes which schema applies at a position, so `collect_schemas` and `collect_child_schemas` in `crates/taplo-common/src/schema/mod.rs` are untouched. The applicator keywords that do change traversal are the tracking issue's next section. Hover sees only the schemas `collect_schemas` yields: a constraint written beside a `$ref` is dropped with the other siblings when the reference is followed, and one inside `if`, `then`, `else`, `not`, `contains`, `propertyNames` or `dependentSchemas` is never reached. Both are the tracking issue's `$ref` and applicator sections.

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

`render` does not change. The tracking issue asks for a decision on how several constraints on one key render together; the decision is one flat bullet list per schema, one line per fact, paired bounds on one line, in a fixed order, filtered by the declared type. The previous feature set expected the decision to land as a change to `render`; it lands as none, because three mechanisms keep the list short at the source:

1. **Paired bounds collapse onto one line.** Five keyword pairs become at most five bullets instead of ten, and a pair whose bounds are equal becomes one bare value.
2. **Facts are filtered by the declared type.** A keyword that cannot bite is not rendered, so a string key never shows a numeric range and an integer key never shows a pattern.
3. **Fact order is fixed**, so the popup reads the same way for every schema.

The filter makes the bullet count a function of the declared type. A string key that writes every keyword applying to a string reaches nine bullets, a number or an array six, an object five; fourteen needs a typeless schema that writes every keyword, which describes a value that is a string, a number, an array and an object at once. Nine one-line bullets is a list, and a heading per group would add a line to every popup to save none. A hard cap was rejected: it would hide whichever constraint happened to sort last, and the reader has no way to ask for the rest. A popup over a key that several schemas apply to renders one list per schema separated by rules; that shape is the previous feature set's and is unchanged here.

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

`Unique items` is the only constraint fact with no values, so it renders as a bare `- Unique items`; `render` already handles that case. `Items` and `Properties` name counts, which the comparison in the value makes unambiguous; `Unique items` beneath `Items` shares the noun on purpose.

### Bounds

A bounded pair renders as one fact whose values are the comparisons that apply:

| Schema | Rendered |
|---|---|
| `minimum: 1`, `maximum: 10` | ``- Range: `>= 1`, `<= 10` `` |
| `minimum: 1` | ``- Range: `>= 1` `` |
| `exclusiveMinimum: 0`, `maximum: 10` | ``- Range: `> 0`, `<= 10` `` |
| `minLength: 40`, `maxLength: 40` | ``- Length: `40` `` |
| `maxItems: 3` | ``- Items: `<= 3` `` |

ASCII comparison operators rather than interval notation. `[1, 10]` is a TOML array literal, and a hover popup over a TOML document is the one place that collision is guaranteed to be read the wrong way. `>=` also carries its meaning one-sided, which interval notation does not: `[1,` is not a thing to write. Prose (`1 to 10`, `at least 1`) needs a second form for exclusive bounds and a third for one-sided ones, and Unicode `≥`/`≤` inside a code span depends on the client font for a glyph that ASCII spells without loss.

Both bounds inclusive and equal collapse to the bare value, because `Length: >= 40, <= 40` is a fixed-width string stated in the least readable way available. The rule is shared by all four bounded facts rather than special-cased for lengths; a `Range` of exactly one number is rare but reads the same way. Equality is `serde_json::Number` equality, which compares an integer and a float as unequal, so `40` and `40.0` do not collapse and render as written. Two exclusive bounds never collapse: `> 5`, `< 5` is an empty range and is stated as the author wrote it.

Two spellings of an exclusive bound are honored, distinguished by shape rather than by draft:

- A **number** in `exclusiveMinimum` or `exclusiveMaximum` is the draft-6 form: the bound itself.
- A **boolean** `true` in `exclusiveMinimum` beside a `minimum`, or in `exclusiveMaximum` beside a `maximum`, is the draft-4 form: a modifier that makes the neighbouring bound exclusive. A boolean with no neighbouring bound modifies nothing and contributes nothing.

A schema may write both `minimum` and a numeric `exclusiveMinimum`, or both `maximum` and a numeric `exclusiveMaximum`. Every bound written renders, lower bounds before upper bounds and `minimum` before `exclusiveMinimum` within a side, so `{"minimum": 5, "exclusiveMinimum": 0}` is ``- Range: `>= 5`, `> 0` ``. Hover states the schema and does not compute the tighter bound, which would need numeric comparison across `u64`, `i64` and `f64` to answer a question the author already answered twice. The collapse applies only when each side carries exactly one bound.

`declared_draft` classifies `Draft4` (`crates/taplo-common/src/schema/mod.rs:732`), so a draft-4 schema is one Taplo validates, and rendering `>= 1` for a schema that means `> 1` would be wrong rather than merely incomplete. The two forms cannot collide: draft 4 defines the keyword as a boolean and draft 6 onwards as a number, so the JSON type settles which was meant without consulting `$schema`. Under draft 6 and later `jsonschema` rejects a boolean in either keyword at compile time, so such a schema produces no diagnostics at all; hover still renders the exclusive bound, because that is what the author wrote.

A `minimum` that is not a number, or an `exclusiveMinimum` that is neither a number nor a boolean, contributes nothing. Bounds render through `serde_json::Number`'s own `Display`, not through `f64`: an integer within `i64` or `u64` range is exact and carries no fractional part, a bound written with a fraction or an exponent renders in shortest round-trip form (`10.0`, and `1e2` as `100.0`), and an integer beyond `u64` range has already been read as a float by `serde_json` and renders as one.

### The type filter

```rust
/// Whether a schema's declared `type` admits instances of `wanted`, and so
/// whether a keyword constraining `wanted` can ever fire.
///
/// A schema that declares no type, or declares one in a shape the
/// specification does not describe, admits everything: the author wrote the
/// keyword, and hover has nothing better to go on.
fn admits_type(schema: &Value, wanted: &str) -> bool
```

`type` may be a string, an array of strings, or absent. An array matches when any member matches. `integer` counts as `number`, since the numeric keywords constrain both.

The filter removes only what the keyword's own definition makes vacuous, which is why it needs no draft and why it is not in tension with rendering an unenforced `format`: `minLength` on an integer is dead by the definition of `minLength`, whereas `uri-template` under 2020-12 is live by the definition of `format` and unenforced only by this build of `jsonschema`. The same reasoning licenses nothing else. An annotation such as `default` carries no applicability condition and is never filtered; an implementation gap such as an uncompilable `pattern` is not a fact about the schema and is never filtered.

`collect_schemas` yields each `oneOf`, `anyOf` and `allOf` member as its own schema, excludes a parent that carries `allOf`, and follows `$ref` without keeping the referring object's siblings. It never merges a parent's `type` into a child, so the only `type` the filter can see is one the author wrote in the same object as the keyword. A composed schema whose `type` sits in a sibling branch declares no type at the constraint's level, the filter admits everything, and the constraint renders.

### Hover states the schema, not the enforcement

`format`, `contentMediaType` and `contentEncoding` render as plain facts. Hover does not mark which of them Taplo enforces, and does not consult the declared draft.

Three reasons.

**Hover's subject is the schema.** ``- Format: `uri-template` `` says the schema declares that format, which is true under every draft. It is the same statement ``- Pattern: `^v\d+$` `` makes, and hover does not promise there either that Taplo's regular expression engine agrees with ECMA-262 on every construct. A label that promised enforcement would need a per-format, per-draft truth table to stay honest; a label that describes needs nothing.

**The information is what the reader wants either way.** `contentMediaType` is defined as an annotation. Its author wrote it to tell a reader what the string holds. That is exactly hover's job, and it is the only job the keyword has ever had.

**Getting a draft to the renderer costs more than the answer is worth.** `declared_draft` is private to `crates/taplo-common/src/schema/mod.rs`, and the previous feature set left it private deliberately: it classifies the *root* schema, while the schema under the cursor may have arrived through a `$ref` into a document with its own declaration, and `collect_schemas` carries no draft context. Reaching it from `taplo-lsp` needs `pub`, not `pub(crate)`, plus a new accessor on `Schemas` and a draft threaded through `schemas_at_path` into the handler — a traversal change to footnote an annotation. Nothing in `taplo-common` changes.

Two cheaper alternatives fail for the same reason. A static list of enforced formats that consults no draft is wrong for half the schemas, because seven formats assert under draft 7 and not under 2020-12. A label chosen to avoid promising enforcement already exists: `Format` describes, and only a label such as `Validated as` would make the promise the truth table exists to keep.

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

Five bullets for eight keywords, and the same schema with `type` removed adds nothing, because no other keyword is present.

### Shape of the code

Ten contributors, four shared helpers, all in `hover.rs`:

```rust
/// One end of a range: the bound and whether the schema excludes it.
struct Bound { value: Number, exclusive: bool }

/// Renders the bounds on a quantity as one fact: `>= 1`, `<= 10`, or the bare
/// bound when each side carries one inclusive bound and the two are equal,
/// which is how a fixed size reads best.
fn bounds_fact(label: &'static str, lower: Vec<Bound>, upper: Vec<Bound>) -> Option<Fact>

/// Reads a keyword that bounds a size or a count, which no draft spells as
/// exclusive.
fn inclusive_bound(schema: &Value, keyword: &str) -> Option<Bound>

/// A fact whose only value is a string the schema states verbatim, such as a
/// regular expression or a format name.
fn string_fact(schema: &Value, label: &'static str, keyword: &str) -> Option<Fact>
```

`uniqueItems` reuses the existing `flag` helper, which counts only a literal `true`, matching `readOnly` and `writeOnly`. `key_hover_sections` gains four `if admits_type(...)` blocks, so the filter is visible at the call site rather than repeated inside each contributor.

The doc comment on `Fact::values` changes from "Rendered TOML literals" to one that covers comparisons, regular expressions and format names as well, since those are not TOML.

## Behavior changes to accept

**Key hover gains bullets for schemas that carry constraint keywords.** A key that previously produced no hover at all now produces one when its schema declares a constraint and nothing else. `{"type": "string", "minLength": 1}` goes from `None` to ``- Length: `>= 1` ``.

**A constraint keyword contradicting the declared type disappears from hover.** No instance of the declared type could violate it before this feature set either, so nothing is lost; the filter is what stops it from appearing.

**A `format` that Taplo does not enforce is still shown.** Deliberate, and argued above.

## Acceptance criteria

Every criterion is asserted through the real `hover` handler unless it names a helper. The fixture is `hover_at` / `key_hover_for` in `hover.rs`'s `mod tests`.

1. Key hover on `{"type": "integer", "minimum": 1, "maximum": 10}` is ``- Range: `>= 1`, `<= 10` ``, and `{"type": "integer", "minimum": 5, "maximum": 5}` collapses to ``- Range: `5` ``.
2. `{"type": "integer", "exclusiveMinimum": 0, "exclusiveMaximum": 10}` is ``- Range: `> 0`, `< 10` ``; a one-sided `{"type": "integer", "minimum": 1}` is ``- Range: `>= 1` ``.
3. `{"type": "integer", "minimum": 1, "exclusiveMinimum": true}` is ``- Range: `> 1` ``; `{"type": "integer", "maximum": 10, "exclusiveMaximum": true}` is ``- Range: `< 10` ``; `{"type": "integer", "minimum": 1, "exclusiveMinimum": false}` is ``- Range: `>= 1` ``; and `{"type": "integer", "exclusiveMinimum": true}` alone renders nothing.
4. `{"type": "integer", "multipleOf": 5}` is ``- Multiple of: `5` ``.
5. `{"type": "integer", "minimum": 5, "exclusiveMinimum": 0}` is ``- Range: `>= 5`, `> 0` ``.
6. `{"type": "string", "minLength": 1, "maxLength": 128}` is ``- Length: `>= 1`, `<= 128` ``, and `{"type": "string", "minLength": 40, "maxLength": 40}` collapses to ``- Length: `40` ``.
7. `{"type": "string", "pattern": "^v\\d+$"}` is ``- Pattern: `^v\d+$` ``, and a pattern of ``a`b`` is fenced with a doubled backtick and padded, matching what `code_span` already does.
8. `{"type": "string", "format": "semver"}` is ``- Format: `semver` ``, and `format: "uri-template"` renders identically rather than being marked or dropped.
9. `{"type": "string", "contentMediaType": "application/json", "contentEncoding": "base64"}` is ``- Media type: `application/json` `` followed by ``- Encoding: `base64` ``.
10. `{"type": "array", "minItems": 1, "maxItems": 3, "uniqueItems": true}` is ``- Items: `>= 1`, `<= 3` `` followed by `- Unique items`, and a `uniqueItems` that is `false` or not a boolean contributes nothing.
11. `{"type": "object", "minProperties": 1, "maxProperties": 5}` is ``- Properties: `>= 1`, `<= 5` ``.
12. Every keyword written on a schema whose declared `type` it cannot constrain renders nothing: each of the seven schemas in the "dead keyword" table produces `None` from key hover, whatever the document holds.
13. `{"type": ["string", "null"], "minLength": 1}` renders the length and `{"type": ["integer", "null"], "minimum": 1}` renders the range; a schema with no `type` that writes `minimum: 1`, `multipleOf: 2`, `minLength: 1`, `pattern: "^a$"`, `format: "email"`, `contentMediaType: "text/plain"`, `contentEncoding: "base64"`, `minItems: 1`, `uniqueItems: true` and `minProperties: 1` renders all ten facts in the order of the Facts table.
14. A non-numeric `minimum`, a non-string `pattern` and a non-string `format` each contribute nothing.
15. The full worked example above renders exactly the five bullets shown, in that order, under its documentation.
16. `cargo check --workspace --all-targets`, `cargo test --workspace` and `cargo check --target wasm32-unknown-unknown` from `crates/taplo-wasm` are clean, along with the CI command set in `.github/workflows/ci.yaml`: `cargo test -p taplo`; `cargo test -p taplo-common --features schema,reqwest,rustls-tls`; `cargo check`/`cargo test` for `-p lsp-async-stub -p taplo-common -p taplo-lsp -p taplo` and for `-p taplo-cli`; and `cargo run -- fmt --check` followed by a clean `git diff-index --quiet HEAD --`.

## Landing

Three commits on `feat/schema-constraints-hover`, each building and passing on its own. This is one branch in a stack of five, so these are commits rather than separate pull requests; the branch opens one pull request.

1. `feat(lsp): render numeric constraints in hover` — `Bound`, `bounds_fact`, `inclusive_bound`, `admits_type`, and the `Range` and `Multiple of` facts. The shared helpers land with the first facts that need them, and every branch of `bounds_fact` is asserted here: one-sided, two-sided, exclusive, doubled on one side, and the equal-inclusive collapse. The type filter is provable here too: `{"type": "string", "minimum": 1}` renders nothing. The commit body names `admits_type` as the helper the two following commits build on. Criteria 1 through 5, and the numeric rows of 12 through 14.
2. `feat(lsp): render string constraints in hover` — `string_fact`, and the `Length`, `Pattern`, `Format`, `Media type` and `Encoding` facts. Criteria 6 through 9 and 15.
3. `feat(lsp): render array and object constraints` — `Items`, `Unique items` and `Properties`. Criteria 10, 11, and the remaining rows of 12 and 13.

The split is by constrained type because that is the boundary a reviewer can reject one side of: the argument about `format` and the argument about the draft-4 `exclusiveMinimum` are in different commits and share no code beyond helpers that commit 1 justifies on its own. Each commit's test set is self-contained.

## Reproducing the findings

The three probe tables under "Problem" were produced by appending a `#[tokio::test]` to `crates/taplo-common/src/schema/tests.rs` that reuses the `seeded` fixture already there, calls `schemas.validate(&url, &instance)` for each row, prints the error count and ends in `panic!` so the output survives. Run with `cargo test -p taplo-common --features schema,reqwest,rustls-tls <name> -- --nocapture`. The probe was reverted; nothing in `taplo-common` changes.

Verified at `5a49d25`, against `jsonschema` 0.17.1 and `serde_json` 1.0.113.
