# JSON Schema applicator keywords

**Status:** Draft
**Tracking issue:** AbysmalBiscuit/taplo#1, "Applicator keywords" section

## Problem

An applicator keyword decides *which* schema applies to a position. Taplo's traversal reads five of them — `$ref`, `allOf`, `oneOf`, `anyOf`, and the `properties`/`patternProperties`/`additionalProperties`/`items`/`prefixItems` family — and ignores every other one. A schema that routes its shape through `if`/`then`, `dependentSchemas` or `unevaluatedProperties` validates correctly and then hands hover and completion nothing at all.

Probed through the real entry points, with the fixture under "Reproducing the findings". Each row is a schema whose only route to the named key is the applicator under test:

| Schema shape | Key probed | `schemas_at_path` | `possible_schemas_from` |
|---|---|---|---|
| `if` / `then` / `else` | `extra` | none | not offered |
| `not` | `banned` | none | not offered |
| `dependencies` (schema form) | `b` | none | not offered |
| `dependencies` (array form) | `b` | none | not offered |
| `dependentSchemas` | `b` | none | not offered |
| `unevaluatedProperties` | any key | none | not offered |
| `propertyNames` | `abc` | none | not offered |
| `contains` | index `0` | none | not offered |

Eight shapes, nothing found in either direction. `prefixItems` is the ninth item on the tracking issue's checklist and is the exception: the draft-versions feature set closed it in `collect_schemas`, and `prefix_items_resolves_at_covered_index`, `prefix_items_falls_through_to_items_past_the_end` and `items_does_not_apply_at_an_index_prefix_items_covers` in `crates/taplo-common/src/schema/tests.rs` prove it. It is not closed in `collect_child_schemas`, which never walks arrays at all; see "Behavior changes to accept".

### The instance is already there, and nothing reads it

`if` needs the document to decide anything, and the tracking issue asks for exactly that. `collect_schemas` already carries the instance: `schemas_at_path` is handed the whole document as JSON (`serde_json::to_value(&doc.dom)` in both `hover` and `completion`), and every descent indexes it in step with the schema — `&value[k.value()]` beside `schema["properties"][k]`, `&value[idx]` beside the item schema.

Probed by printing `full_path` and `value` where `collect_schemas` pushes a result, for the document `{"server": {"port": 8080, "host": "localhost"}}`:

```
full_path=server.port value=8080
full_path=server      value={"port":8080,"host":"localhost"}
```

The instance at each position is the instance for that position. No traversal code reads it for a decision today.

### A half-typed document yields an absent key, never a null

TOML has no null, so a key the user has not finished typing is missing from the serialized instance rather than present-and-null. Probed with `serde_json::to_value` over `taplo::parser::parse(src).into_dom()`:

| Source | Instance |
|---|---|
| `kind = "docker"\nimage = \n` | `{"kind": "docker"}` |
| `kind = "docker"\n[server]\npo\n` | `{"kind": "docker", "server": {}}` |
| `kind = \n` | `{}` |
| `kind = "a"\nbroken = [1,\n` | `{"kind": "a", "broken": [1]}` |

Two things follow. `Value::Null` is an unambiguous "absent here", which is what makes an undecidable condition detectable. And the discriminator a condition tests is present in the instance from the moment its value parses, which is what makes evaluating a condition mid-edit useful rather than noisy.

### `collect_schemas` still overflows the stack

`3cadcb4` gave `collect_child_schemas` a composition budget so that a cycle through `$ref`, `allOf`, `oneOf` or `anyOf` terminates. `collect_schemas` never got one. It recurses on `$ref` and on every `allOf`/`oneOf`/`anyOf` member without consuming any path, so the same cycle runs forever.

Probed with the schema from the existing `self_referential_all_of_terminates` test — which passes, because it queries the empty path and `collect_schemas` returns before descending — at the non-empty path `node`:

```
thread 'schema::tests::probe_collect_schemas_cycle' has overflowed its stack
fatal runtime error: stack overflow, aborting
```

Hovering a key whose schema is a self-referential `allOf` kills the language server today. This is inherited, not introduced here, but it has to be fixed first: `if`/`then` and `dependentSchemas` are in-place applicators that recurse with the path unchanged, exactly like `allOf`, so every one of them is a new route into the same cycle.

### A condition that names a `$ref` evaluates to false, silently

`create_validator` compiles any `Value`, so a condition subschema can be compiled and run on its own. Probed:

| Subschema | Result |
|---|---|
| `{"properties": {"kind": {"const": "a"}}, "required": ["kind"]}` | compiles; `{"kind":"a"}` valid, `{"kind":"b"}` invalid, `{}` invalid |
| `{"properties": {"kind": {"$ref": "#/definitions/a"}}, "required": ["kind"]}` | compiles; `{"kind":"a"}` **invalid**, `{"kind":"b"}` invalid |
| `{"$ref": "https://example.com/nope.json"}` | compiles; every instance invalid |
| `{"type": "string", "pattern": "("}` | compile fails: `invalid schema: "(" is not a "regex"` |
| `true` / `false` | compiles; valid / invalid |
| 1000 compiles of the first row | 33.3 ms, so about 33 µs each |

The second row is the trap. A subschema lifted out of its document keeps its `#/...` pointers but loses the document they point into, so resolution fails and *every* instance comes back invalid. A condition written as `{"if": {"$ref": "#/definitions/isPostgres"}}` would therefore always pick `else`, confidently and wrongly. Detecting that case matters more than handling it.

### Yielding a subschema is a claim about the user's obligation

Hover renders the constraint keywords of every schema `collect_schemas` yields, one bullet list per schema separated by `---`, and completion offers every yielded schema's `properties` as keys and its `enum`/`default`/`examples` as values. Both read a yielded schema as "this is what must hold here".

That reading is right for `then` and wrong for four of the keywords on the checklist:

| Keyword | What the subschema says | What yielding it would claim |
|---|---|---|
| `then` | this schema applies to this instance | correct |
| `not` | the instance must **not** match this | the instance must match it |
| `propertyNames` | every **key name** matches this | every **value** matches it |
| `contains` | **some** element matches this | **this** element matches it |
| `dependentRequired` | these keys become required | (names keys, not a schema) |

So the answer to "what does traversal yield for `not`" is *nothing*, and the same for `propertyNames` and `contains`. Their content still has to reach the reader; it reaches them at the schema that writes them, through hover, and not by pretending a prohibition is a requirement.

## Goals

- `if`/`then`/`else` picks its branch from the document, and the branch's keys and values reach completion and hover.
- `dependencies` in its schema form and `dependentSchemas` apply when their trigger key is present.
- A key that no other applicator in its schema evaluates picks up `unevaluatedProperties`.
- `not`, `propertyNames` and `contains` reach the reader as what they are, and never as a requirement on the value at the cursor.
- Traversal terminates on every cyclic schema, including the ones the new in-place applicators make reachable.
- Nothing already collected stops being collected.

## Non-goals

**`unevaluatedItems`.** `jsonschema` 0.17.1 has no implementation of it. `src/keywords/` contains `unevaluated_properties.rs` and no `unevaluated_items.rs`, and `Draft::get_validator` in `src/schemas.rs` has no `"unevaluatedItems"` arm under any draft or feature. There is nothing to mirror and nothing to validate against, so a traversal rule for it could not be checked against the validator's own verdict. Closing it needs the upgrade to `jsonschema` 0.55, which renames the core type and replaces the `SchemaResolver` trait `CacheSchemaResolver` implements; the draft-versions spec already defers that upgrade to its own feature set. The checklist item stays unchecked.

**`minContains` / `maxContains`.** Not on the checklist, and `Draft::get_validator` has no arm for either, so they neither validate nor have anything to render.

**Validation.** Every keyword here already validates, under the drafts the draft-versions feature set enabled. No diagnostic changes.

**`$ref` inside a condition.** A condition that names a reference is detected and treated as undecidable rather than resolved. Resolving it needs the reference work in the tracking issue's `$ref` section, which is the next feature set.

**Marking a completion item as required.** `dependentRequired` and the array form of `dependencies` name keys that become required. Completion has no notion of required today — no item is marked, from `required` either — so adding one for the conditional case would be a completion-wide design decision made from its rarest input.

**Telling the reader that a `then` schema is conditional.** A branch traversal selected applies to the document as it stands, which is what hover states. The `(Keys, Arc<Value>)` a yield carries has no room for a provenance note, and adding one changes a signature three feature sets sit on to footnote something the reader can see in the schema.

## Design

### One rule for the instance

Every in-place applicator that depends on the document follows the same rule:

> Evaluate against the instance at this position when that instance is present. When it is absent, or the condition cannot be decided, take every branch.

`Value::Null` means absent, which the probe above establishes for TOML. Taking every branch is what traversal already does for `oneOf` and `anyOf`: the consumers union what they are given, hover separating alternatives with `---` and completion offering the union of the keys. Over-offering is the failure mode a user can work around; under-offering is the one that reads as completion being broken, which is the tracking issue's own framing.

This rule is what lets both traversal functions behave consistently. `collect_schemas` walks the path the cursor is on, where the instance is almost always present, so it decides. `collect_child_schemas` walks *hypothetical* keys the user has not written yet, where the instance is almost always absent, so it offers both. Neither needs a special case, and the two never contradict each other at the position they share.

### `collect_child_schemas` gains the instance

`collect_schemas` already has it. `collect_child_schemas` does not, and without it the two functions would disagree at the cursor: `collect_schemas` would yield `then` alone, and `collect_child_schemas`, running over that same parent schema, would put `else` back.

`possible_schemas_from` has the document and the path of every schema `schemas_at_path` returned, so it walks one to the other:

```rust
/// The part of the document a schema at `keys` applies to.
///
/// Indexing a `Value` with a missing key or an out-of-range index yields
/// `Value::Null`, which is how an absent position reports itself.
fn instance_at<'v>(value: &'v Value, keys: &Keys) -> &'v Value
```

`collect_child_schemas` takes that as a parameter, passes it unchanged through every in-place descent (`$ref`, `oneOf`, `anyOf`, the composed-`allOf` merge, `then`, `else`, a dependent schema) and indexes it on each property descent, exactly as `collect_schemas` does.

### `if` / `then` / `else`

Handled beside `allOf`, `oneOf` and `anyOf` at the top of both functions, because none of the three consumes a path segment.

1. No `if`, or neither `then` nor `else`: nothing happens. A `then` or `else` without an `if` is inert per the specification and stays inert here.
2. The instance is `Value::Null`, or the condition subschema contains a `$ref` anywhere within it, or compiling it fails: **undecidable**. Descend into `then` and into `else`, both with the path unchanged.
3. Otherwise compile the condition and run the instance through it. Descend into `then` if it is valid, into `else` if it is not.

The schema that carries the `if` keeps being included in its own right, the way a schema carrying `oneOf` does. `then` states what is *additionally* true, not what replaces the parent.

The condition is compiled through the same builder `create_validator` uses — the cache resolver, the two registered formats, `should_validate_formats(true)` — and against the root document's declared draft rather than the crate default, so a condition is evaluated under the same rules that validate it. `declared_draft` and `create_validator` are both private to `crates/taplo-common/src/schema/mod.rs`, which is where traversal lives, so nothing changes visibility.

Compiling costs about 33 µs. It happens only in case 3, which requires a present instance, so the number of compiles a request pays for is bounded by how much of the document the traversal actually walks and not by how large the schema is. A schema with an `if` on every one of a thousand properties costs nothing extra to complete against an empty document. If profiling ever disagrees, the hook is an LRU keyed by `ArcHashValue` beside the existing validator cache; this spec does not add one for a cost it cannot measure.

**Why a `$ref` in the condition is refused rather than resolved.** The probe above shows the failure is silent and total: the condition returns false for every instance, so `else` wins every time and nothing in the output says why. A recursive scan for a `$ref` key turns that into the undecidable case, where both branches are offered and the reader sees both. The scan is over a condition subschema, which is small; the cost is not worth measuring.

**Stability while typing.** The branch follows the document, which is the behavior the tracking issue asks for, and it does move as the user types. Writing `kind = ` and stopping leaves `kind` absent from the instance, so a condition with `required: ["kind"]` fails and `else` applies until the value parses. That is the same verdict validation gives the same half-typed document, and it is not the case that matters: the completion the user wants at that moment is the *value of the discriminator*, which comes from `properties.kind` and not from either branch. By the time the branch matters — the user has moved on to the next key — the discriminator has parsed.

### `dependencies` and `dependentSchemas`

`dependencies` carries two shapes under one keyword, and `dependentSchemas` is the 2019-09 spelling of the schema shape:

- **Schema form** (`{"dependencies": {"a": {…}}}`, `{"dependentSchemas": {"a": {…}}}`): the subschema applies to the object when key `a` is present. Handled as an in-place applicator, like `then`: when the instance is an object containing the trigger key, descend with the path unchanged. When the instance is absent, descend into every dependent subschema. When the instance is present and the trigger key is not, descend into nothing.
- **Array form** (`{"dependencies": {"a": ["b", "c"]}}`, and all of `dependentRequired`): names keys, not a schema. There is no subschema to yield and traversal does nothing with it. It validates, and a violation is a diagnostic.

Both keywords are read wherever they appear, without consulting the draft, which is how the rest of traversal already works: `collect_schemas` reads `prefixItems` and draft-7 tuple `items` side by side, and the draft-versions spec settled that traversal is deliberately draft-agnostic.

### `unevaluatedProperties`

The specification defines `unevaluatedProperties` against annotations: it applies to the properties that no other applicator in the same schema — including the ones reached in place through `$ref`, `allOf`, `oneOf`, `anyOf` and `if`/`then`/`else` — evaluated. Traversal collects no annotations, so an exact implementation is not available. What is available is a close reading of the same idea.

`collect_schemas` accumulates its results into one vector, and every entry it adds while resolving one call is for the path that call is walking. So the approximation is:

> Record the length of the accumulator on entry. After the schema's own `properties`, `patternProperties` and `additionalProperties` have had their turn at a key — and after the composition branches at the top of the function, which run first and push into the same accumulator — descend into `unevaluatedProperties` only if nothing was added.

Which is: *no applicator this traversal followed produced a schema for this key*. That is exact for the shapes `unevaluatedProperties` exists to serve. A schema carrying `unevaluatedProperties` beside `allOf`, `oneOf`, `anyOf` or `if`/`then`/`else` gets the right answer, because those branches run before the check and push into the accumulator when they match. A branch that carries its own `unevaluatedProperties` gets the right answer for itself, because each recursion measures its own contribution.

It diverges in one place: traversal returns at `$ref` without reading the referring object's siblings, so `{"$ref": "…", "unevaluatedProperties": {…}}` loses the keyword along with every other sibling. That gap is inherited, it is the tracking issue's `$ref` section, and it belongs to the next feature set.

A boolean `unevaluatedProperties` needs no special case. `false` is the common spelling and means the key is forbidden; `collect_schemas` returns immediately for a schema that is not an object, so nothing is yielded, which is the right answer for a forbidden key.

`collect_child_schemas` gets no `unevaluatedProperties` handling. It builds completion items out of key names read from `properties`, and `unevaluatedProperties` names no key.

### `not`, `propertyNames` and `contains` yield nothing

None of the three describes the value at a child position, so none is descended into. Each is instead stated by hover at the schema that writes it, as a labelled block whose contents are the subschema's own facts:

```
- Must not match
  - Type: `string`
  - Pattern: `^v\d+$`
- Key names
  - Pattern: `^[a-z][a-z0-9_-]*$`
- Contains
  - Const: `"required-element"`
```

The label is what makes the block honest. `Pattern: ^v\d+$` under `Must not match` is a prohibition; the same line at the top level is a requirement; the reader is told which.

`not` deserves the argument spelled out, because descending into it is the tempting mistake. `{"not": {"properties": {"banned": {"type": "string"}}}}` does not forbid a key named `banned`. It forbids the object from matching the whole subschema, and an object with `banned = 1` satisfies the parent perfectly well. So there is no negated schema to attach to any child position — not even a negated one. `not` removes possibilities, and traversal has no way to express removal: it yields a set that every consumer unions.

`propertyNames` constrains names and could in principle filter the key completions traversal produces. It would filter nothing: those keys come from `properties`, which are literal names the schema author wrote, and an author who writes a name their own `propertyNames` rejects has written a schema no instance can satisfy. Rendering it tells the reader what to name the keys `properties` does not list, which is the case the keyword is for.

`contains` says at least one element matches. Offering its subschema at index `0` would tell a user that every element must match it. Hover states it on the array.

### Hover renders a nested block without disturbing the flat one

`HoverSections` gains one field:

```rust
/// A subschema this schema names in a role that is not "the value here":
/// a prohibition, a rule for key names, a rule some element must satisfy.
/// Rendered under the flat facts, as a labelled bullet with the subschema's
/// own facts indented beneath it.
struct NestedFacts { label: &'static str, facts: Vec<Fact> }
```

`Fact` is untouched, so none of the ten existing contributors changes. `render` grows one loop that emits `- {label}` followed by each nested fact indented two spaces, inside the same bullet block as the flat facts, so no blank line opens between them. The blocks come last, after `Read-only` and `Write-only`: they describe something other than the key, and the reader should meet them after everything that describes the key.

The nested facts come from one helper:

```rust
/// What a subschema states about itself, for a reader who has been told the
/// role it plays. `type`, `const` and `enum` appear here and not among a
/// schema's own facts because a prohibition or a key-name rule is very often
/// nothing but one of the three, whereas at the top level the value the user
/// wrote already says which type it is.
fn subschema_facts(schema: &Value) -> Vec<Fact>
```

It emits `Type`, `Const`, `One of` (from `enum`) and `Required`, then the same constraint facts a schema's own hover shows. Those constraint facts are lifted out of `key_hover_sections` into `fn constraint_facts(schema: &Value) -> Vec<Fact>` and called from both places, which is a refactor with no behavior change: the facts, their order and their type filter are exactly as the constraints feature set left them.

A subschema that produces no facts produces no block. `{"not": {"minProperties": 1}}` on a string-typed schema renders nothing, because the filter finds the keyword vacuous, and a block with an empty body would be worse than silence.

**The type filter's justification is unchanged.** `admits_type` reads the `type` written in the same object as the keyword, and the constraints feature set justified that by the fact that `collect_schemas` never merges a parent's `type` into a child. Nothing here merges a type into anything. `subschema_facts` applies `admits_type` to the subschema it was handed, which is the object those keywords are written in; a `not` subschema declaring `"type": "string"` filters by `string`, as the author wrote it. Every schema this feature set newly yields — `then`, `else`, a dependent schema, an `unevaluatedProperties` schema — is yielded whole and unmerged, exactly as `oneOf` members already are.

### Termination

`collect_schemas` gets the budget `3cadcb4` gave `collect_child_schemas`, for the reason `3cadcb4` gives: a cycle through an in-place applicator makes no progress against anything the function already counts, and `collect_schemas` counts nothing at all. Every in-place descent — `$ref`, `allOf`, `oneOf`, `anyOf`, and now `then`, `else` and a dependent schema — consumes one unit of `MAX_COMPOSITION_DEPTH`. Every descent that consumes a path segment resets it, because the path is finite and shrinking, which is the same place `collect_child_schemas` resets it.

That is what keeps the new keywords from re-opening `3cadcb4`. They add three more ways to recurse without consuming a path segment, and all three are counted by the one budget that exists for exactly that.

## Behavior changes to accept

**Hovering a cyclic schema stops crashing the language server.** A self-referential `allOf` reached at a non-empty path overflows the stack today. It will return the schemas found within the budget instead.

**A composition chain deeper than 32 in-place steps between two path segments is truncated.** The same limit `collect_child_schemas` has carried since `3cadcb4`, now applied to the other traversal. Thirty-two `$ref`/`allOf` hops without descending into a single property is a schema nobody writes; a cycle is what actually reaches the limit.

**Hover gains blocks, and both hover and completion gain schemas.** A key routed through `then`, a dependent schema or `unevaluatedProperties` produces hover text and completion items where it produced none. A schema carrying `not`, `propertyNames` or `contains` gains a hover block. All of it is the point of the change, and all of it will read as new noise to someone whose schema was quietly half-read before.

**A condition the document cannot decide shows both branches.** Hover over a key inside a `then` whose `if` names a `$ref` shows the `then` block and the `else` block separated by `---`. That is the same shape `oneOf` has always produced.

**`prefixItems` still does not reach completion.** `collect_child_schemas` walks `properties` and nothing else — not `items`, not `prefixItems`, not any array keyword — so the draft-versions feature set's `prefixItems` support reaches `schemas_at_path` and stops there. Teaching completion to enumerate array positions is not an applicator problem: a completion item is a key name, and an array index is not one. Unchanged here, and named so the next reader does not mistake it for a gap this feature set opened.

## Acceptance criteria

Traversal criteria run against `schemas_at_path` and `possible_schemas_from` in `crates/taplo-common/src/schema/tests.rs`. Rendering criteria run against the real `hover` handler through `hover_at`, and completion criteria against the real `completion` handler through `complete_at`, both in `crates/taplo-lsp/src/handlers/hover.rs`'s `mod tests`.

1. `schemas_at_path` at the non-empty path `node`, on the self-referential `allOf` schema from `self_referential_all_of_terminates`, returns without overflowing the stack.
2. Given `{"if": {"properties": {"kind": {"const": "docker"}}, "required": ["kind"]}, "then": {"properties": {"image": {…}}}, "else": {"properties": {"image": {…}}}}`, `schemas_at_path` at `image` against the instance `{"kind": "docker"}` returns the `then` schema and not the `else` schema, and against `{"kind": "podman"}` returns the `else` schema and not the `then` schema.
3. The same schema with the instance `Value::Null` returns both branches.
4. A condition containing a `$ref` at any depth returns both branches, rather than the `else` branch alone.
5. A condition that does not compile — `{"if": {"pattern": "("}}` — returns both branches.
6. `then` or `else` without an `if` yields nothing; the schema carrying the `if` is still yielded in its own right alongside the selected branch.
7. Hover on a key reached through `then` renders that branch's documentation, through the real handler, on a document whose discriminator selects it.
8. Completion on a partial key offers the keys of the selected branch and not the keys of the other, through the real handler.
9. `dependencies` in schema form and `dependentSchemas` both yield their subschema at the object's path when the trigger key is present in the instance, and neither yields it when the instance is present without the trigger key.
10. Both yield every dependent subschema when the instance is absent.
11. `dependencies` in array form, and `dependentRequired`, yield nothing.
12. `unevaluatedProperties` yields its schema for a key that `properties`, `patternProperties` and `additionalProperties` do not cover, and does not yield it for a key any of the three covers.
13. `unevaluatedProperties` does not yield for a key an `allOf`, `oneOf`, `anyOf` or selected `if` branch covers, and does yield for a key only the *unselected* branch covers.
14. A boolean `unevaluatedProperties` yields nothing.
15. `not`, `propertyNames` and `contains` yield nothing: the negated subschema's `properties` are not offered as completion keys, its constraint keywords do not appear in hover as requirements, `propertyNames` is not returned as the schema for a value, and `contains` is not returned as the schema for an array index.
16. Hover renders `Must not match`, `Key names` and `Contains` as labelled blocks with the subschema's facts indented beneath, after the flat facts, with no blank line between the flat list and the blocks.
17. A subschema with no renderable facts produces no block.
18. Every schema `schemas_at_path` and `possible_schemas_from` returned before this feature set is still returned: the 15 existing tests in `crates/taplo-common/src/schema/tests.rs` and the 51 in `cargo test -p taplo-lsp --lib handlers::hover` pass unchanged.
19. `cargo check --workspace --all-targets`, `cargo test --workspace`, `cargo test -p taplo-common --features schema,reqwest,rustls-tls` and `cargo check --target wasm32-unknown-unknown --manifest-path crates/taplo-wasm/Cargo.toml` are all clean.

## Landing

Five commits on `feat/schema-applicators`, each building and passing on its own. This is one branch in a stack of five, so these are commits rather than separate pull requests; the branch opens one pull request.

1. `fix(schema): bound composition depth in collect_schemas` — the stack overflow, with the probe from "Problem" turned into a regression test. First, because every in-place applicator that follows is a new route into the cycle it closes. Criterion 1.
2. `feat(schema): pick the applicable if/then/else branch` — condition compilation against the root's draft, the `$ref` scan, the undecidable rule, `instance_at`, and the instance parameter on `collect_child_schemas`. Criteria 2 through 8.
3. `feat(schema): apply schemas a present key depends on` — `dependencies` in schema form and `dependentSchemas`, on the in-place machinery commit 2 lands. Criteria 9 through 11.
4. `feat(schema): fall back to unevaluatedProperties` — the accumulator check. Criteria 12 through 14.
5. `feat(lsp): render not, propertyNames and contains` — `NestedFacts`, `subschema_facts`, the `constraint_facts` extraction, and the traversal tests that prove the three keywords yield nothing. Criteria 15 through 17.

The split is by keyword because that is the boundary a reviewer can reject one side of. The argument about what `not` yields shares no code with the argument about how `unevaluatedProperties` approximates annotations, and commit 2's instance threading is the only thing commits 3 and 4 borrow.

## Reproducing the findings

The traversal and validator tables came from `#[tokio::test]`s appended to `crates/taplo-common/src/schema/tests.rs`, reusing the `seeded` and `descriptions` helpers already there, printing into a string and ending in `panic!` so the output survives. Run with `cargo test -p taplo-common --features schema,reqwest,rustls-tls <name> -- --nocapture`.

The instance-threading table needed a temporary `println!` beside the `schemas.push` in `collect_schemas`, since nothing reads the instance today and it is therefore not observable from outside.

The DOM serialization table came from a `#[tokio::test]` in `crates/taplo-lsp/src/handlers/hover.rs` calling `serde_json::to_value(&taplo::parser::parse(src).into_dom())`.

Every probe was reverted; the baseline at `777833a` is unchanged.

Verified against `jsonschema` 0.17.1 at Taplo commit `777833a`.

## Open questions

1. Should `dependentRequired` and the array form of `dependencies` render in hover at all — for instance as a `Required with` fact on the object that carries them — or is "validates, nothing to traverse, nothing to render" the honest close for that half of the checklist item?
2. Is "take every branch when the instance is absent" right for `collect_child_schemas`, or should an absent instance mean "take no branch" so that completion never offers a key from a branch the document has ruled out? The two differ only for a schema whose discriminator is written but whose *sub*-object is not yet.
3. Should a condition whose subschema is *itself* a bare `{"$ref": …}` be resolved — traversal already has `ref_schema_value` and the resolution is one `await` — rather than falling into the undecidable case with the rest?
4. `subschema_facts` adds `Type`, `Const`, `One of` and `Required` facts that a schema's own hover does not show. Is that asymmetry worth the legibility, or should the nested blocks show only the constraint keywords the constraints feature set already renders?
5. Is five commits the right split, or should commits 3 and 4 fold into one "the remaining object applicators"?
