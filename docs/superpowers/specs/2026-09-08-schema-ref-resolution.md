# JSON Schema `$ref` resolution

**Status:** Accepted
**Tracking issue:** AbysmalBiscuit/taplo#1, "`$ref` resolution" section

## Problem

A `$ref` names another schema by URI reference. Resolving one is two steps: work out the
absolute URL the reference denotes, and fetch what sits at that URL. Taplo has two
implementations of the first step. `jsonschema` does it inside a `pub(crate)` resolver
that follows RFC 3986 and tracks `$id`. Traversal does it in `reference_url`, eight lines
that recognize two shapes and reject everything else:

```rust
fn reference_url(root_url: &Url, reference: &str) -> Option<Url> {
    if !reference.starts_with('#') {
        return Url::parse(reference).ok();
    }
    let mut url = root_url.clone();
    url.set_fragment(Some(reference.trim_start_matches("#/")));
    Some(url)
}
```

Probed against the base `file:///taplo-test/schema.json`:

| Reference | `reference_url` produces |
|---|---|
| `#/definitions/port` | `file:///taplo-test/schema.json#definitions/port` |
| `#/$defs/port` | `file:///taplo-test/schema.json#$defs/port` |
| `#` | `file:///taplo-test/schema.json##` |
| `#foo` | `file:///taplo-test/schema.json##foo` |
| `common.json#/definitions/port` | none |
| `common.json` | none |
| `../shared/common.json#/definitions/port` | none |
| `/abs/common.json` | none |
| `https://example.com/x.json#/definitions/port` | `https://example.com/x.json#/definitions/port` |
| `file:///other/x.json` | `file:///other/x.json` |

Two facts the table makes visible. A relative reference produces `None`, which
`collect_schemas` turns into `Err("could not determine schema URL")`. And the fragment a
same-document reference produces has its leading `/` stripped, while the fragment an
absolute reference produces keeps it, because one goes through `trim_start_matches("#/")`
and the other through `Url::parse`. `resolve_schema` then prepends a `/` to whatever it
gets:

```rust
let ptr = String::from("/") + fragment;
```

So `#/definitions/port` becomes the pointer `/definitions/port` and works, and
`https://example.com/x.json#/definitions/port` becomes `//definitions/port`, which
`serde_json`'s `pointer` reads as the key `definitions` inside the key `""` and does not
find.

### What each shape actually does, end to end

Every row is `schemas_at_path` and `validate` run against the same seeded pair of
documents, through the real entry points. The fixture is under "Reproducing the findings".

| Reference shape | Traversal | Validation |
|---|---|---|
| `#/definitions/port` | resolves | resolves |
| `#/$defs/port` | resolves | resolves |
| `common.json#/definitions/port`, root has no `$id` | `Err("could not determine schema URL")` | ``Err("the scheme `json-schema` is not supported")`` |
| `common.json#/definitions/port`, root `$id` absolute | `Err("could not determine schema URL")` | resolves |
| `file:///taplo-test/common.json#/definitions/port` | `Err("failed to resolve relative schema")` | resolves |
| `#` | `Err("failed to resolve relative schema")` | resolves |
| `#port`, matching `{"$id": "#port"}` | `Err("failed to resolve relative schema")` | resolves |
| inner `{"$id": "defs/", "$ref": "port.json"}` | `Err("could not determine schema URL")` | resolves |

Five shapes fail in traversal that the tracking issue does not list, and the issue's own
summary of `reference_url` — "handles only `#/...` fragments against the root URL and
fully absolute URLs" — is too generous by one case: a fully absolute URL carrying a JSON
pointer does not work either.

`$defs` is the opposite surprise. It already resolves, under every draft, because
traversal applies the fragment as a pointer to the raw JSON document and never asks a
draft what `$defs` means. The checklist item asks for confirmation, and confirmation is
what it needs: a regression test, not an implementation.

### The failure is an error, not an empty result

`collect_schemas` returns `Result`, and an unresolvable reference is the `Err` arm. Every
handler treats that as nothing at all:

```rust
Ok(s) => s,
Err(error) => {
    tracing::error!(?error, "failed to collect schemas");
    return Ok(None);
}
```

`collect_child_schemas` returns `()` and swallows the same failure inside
`ref_schema_value`, logging and returning `None`. Probed on a schema with one good key and
one key behind a relative reference: `schemas_at_path` at `good` returns its schema,
`possible_schemas_from` at the root returns `["", "good", "bad"]`, and `schemas_at_path`
at `bad` errors. So the damage is confined to the path that crosses the bad reference, and
on that path it is total — no hover, no completion, no link. Which is the tracking issue's
framing exactly: completion that reads as broken at random.

### Validation is not the working half

The tracking issue says the validator "resolves more than that through its own resolver".
It does, conditionally. `create_validator` calls `options.compile(schema)` and never tells
`jsonschema` which URL the schema came from, so the compilation scope is whatever `id_of`
reads from the root:

```rust
let scope = match schemas::id_of(draft, schema) {
    Some(id) => url::Url::parse(id)?,
    None => DEFAULT_ROOT_URL,   // "json-schema:///"
};
```

Three consequences, each probed:

- **No `$id`.** A relative reference resolves against `json-schema:///`, misses the cache
  (which is keyed by the URL taplo fetched from), reaches `CacheSchemaResolver`, comes back
  as `WouldBlockError`, and surfaces as `ValidationErrorKind::Resolver`. `validate_impl`
  then calls `load_schema("json-schema:///common.json")`, whose `fetch_external` reports
  ``the scheme `json-schema` is not supported``. The whole `validate` call errors, so the
  document gets no diagnostics at all.
- **Absolute `$id`.** Everything resolves, including `$id` re-scoping and plain-name
  anchors, because the scope is then the real URL and `CacheSchemaResolver` is asked for
  URLs the cache is keyed by.
- **Relative `$id`,** as in `{"$id": "schema.json"}`. `Url::parse` fails and compilation
  fails: `Err("invalid schema file:///taplo-test/schema.json")`. Again no diagnostics for
  the document.

So the two halves do not divide into a working one and a broken one. They are two partial
implementations that overlap on the one shape everybody writes, `#/definitions/...`.
Fixing traversal alone would move the divergence rather than close it: traversal would
resolve a relative reference that validation still errors on.

### A `$ref` discards its siblings

Both traversals return at `$ref` without reading the object that carries it:

```rust
if let Some(r) = schema.schema_ref() {
    let schema = self.resolve_schema(url).await?;
    return self.collect_schemas(root_url, &schema, /* ... */).await;
}
```

Probed: `{"$ref": "#/definitions/port", "description": "sibling description"}` at `port`
yields `["target description"]`, the sibling gone. And
`{"$ref": "#/definitions/obj", "unevaluatedProperties": {...}}` at `obj.unknown` yields
nothing, the sibling gone. The applicator feature set named the second as the one place its
`unevaluatedProperties` coverage predicate diverges from the specification, and deferred
both here.

`jsonschema` honors siblings from 2019-09 on and ignores them before, by an explicit check
in `compile_validators`:

```rust
let maybe_reference = object.get("$ref")
    .filter(|_| !keywords::ref_::supports_adjacent_validation(context.config.draft()));
```

Since the draft-versions feature set enabled 2019-09 and 2020-12, a modern schema's
`unevaluatedProperties` sibling already validates while traversal drops it.

### A condition holding a nested reference is decidable after all

The applicator feature set found that a condition lifted out of its document keeps its
`#/...` pointers, loses the document they name, and rejects every instance, so
`{"if": {"properties": {"kind": {"$ref": "#/definitions/isDocker"}}, "required": ["kind"]}}`
would silently pick `else` forever. It guarded that with `names_a_ref`, which declares such
a condition undecidable, and left relaxing it to this feature set.

The relaxation does not need inlining. It needs the reference to be absolute. Probed, with
the document already in the cache `CacheSchemaResolver` reads:

| Condition, written under `kind` | `{"kind":"docker"}` | `{"kind":"podman"}` |
|---|---|---|
| `{"$ref": "#/definitions/isDocker"}` | invalid | invalid |
| `{"$ref": "file:///taplo-test/schema.json#/definitions/isDocker"}` | **valid** | invalid |

Rewriting the reference strings inside a condition to their absolute form is a walk over
the condition's own JSON, with no resolution, no target fetch and no recursion into
anything the condition does not contain. The document then arrives through the resolver
the compiler already has, at no clone and no extra request.

### The composition budget bounds depth, not work

`MAX_COMPOSITION_DEPTH` is 32, and every in-place descent spends one unit. A `$ref` hop
costs a unit and its target costs another, so a cycle runs sixteen rounds, and a
*branching* cycle costs its fan-out to that power. Measured on the current traversal, debug
build:

| Schema | Call | Time |
|---|---|---|
| `anyOf` of two references back to itself | `possible_schemas_from` | 531 ms |
| `anyOf` of three references back to itself | `schemas_at_path` | 63.3 s |

Both terminate. Sixty-three seconds inside a language-server request is a hang. The
applicator feature set named the fix — a set of `$ref` URLs visited along one in-place
chain — and left it to the feature set that touches reference resolution.

## Goals

- A `$ref` resolves the way RFC 3986 says: against a base, whatever shape it takes.
- The base moves with `$id`, and a reference resolves against the base in force where it is
  written.
- A plain-name fragment resolves to the subschema that claims it.
- Keywords written beside a `$ref` apply at the same position as its target.
- Validation resolves against the URL the schema was loaded from, so the two halves agree
  on every shape rather than on one.
- A condition holding a nested reference decides its branch instead of being refused.
- A traversal request over a branching cycle finishes in the time a keystroke allows.
- `$defs` is confirmed as a pointer target under every draft.
- Nothing already resolved stops resolving.

## Non-goals

**Draft-4 `id`.** Draft 4 spells the base-changing keyword `id` rather than `$id`, and
`jsonschema`'s `id_of` switches on the compiled draft to pick between them. Traversal is
deliberately draft-agnostic — the draft-versions feature set settled that, and
`collect_schemas` reads `prefixItems` and tuple `items` side by side because of it — so it
has no draft to switch on. Reading `id` unconditionally is the dangerous half of the
choice: `id` is not a keyword from draft 6 on, so a schema carrying one as leftover
metadata would silently re-base every reference beneath it. Reading `$id` unconditionally
costs a draft-4 schema its re-scoping, which is what it has today. `$id` only.

**`$dynamicRef`, `$recursiveRef`, `$dynamicAnchor`, `$recursiveAnchor`.** `jsonschema`
0.17.1 has no arm for any of them, so validation ignores them. Traversal resolving them
would make it disagree with the validator it exists to agree with, in the direction that
invents behavior rather than the direction that reports less.

**Fetching a schema traversal has never fetched.** Resolution decides a URL; `load_schema`
decides what to do with it, and it is untouched. A relative reference under an `https` base
joins to an `https` URL and goes through the same HTTP client an absolute `https` reference
goes through today. Under a `file` base it joins to a `file` URL and goes through
`Environment::read_file`, which is the call `fetch_external` already makes for an absolute
`file` reference. No scheme becomes reachable that was not reachable before, and no host
gains a capability. In wasm that matters concretely: `WasmEnvironment::read_file` delegates
to a `js_read_file` callback the host supplies, so a host that serves absolute file
references serves relative ones by the same callback, and a host that cannot read files
fails a relative reference exactly as it fails an absolute one now.

**Sharing one resolver between traversal and validation.** Argued under "Design".

**`unevaluatedItems`.** `jsonschema` 0.17.1 does not implement it, which the applicator
feature set established and which the upgrade to 0.55 is the only route past. Unchanged.

**The `allOf` carrier that `collect_schemas` discards.** Argued under "Deferred".

## Design

### One mechanism: a base that travels with the schema

The two checklist items — relative file references and `$id` re-scoping — are one mechanism
seen from two sides. A relative reference resolves against a base; `$id` is the rule for how
that base moves. Implementing them separately would implement the same thing twice.

`reference_url` is deleted. In its place every reference resolves by `Url::join`, which is
RFC 3986 reference resolution and already handles all ten rows of the table under "Problem".
Probed against the base `file:///taplo-test/sub/inner.json`:

| Reference | Joined | Fragment |
|---|---|---|
| `#/definitions/port` | `file:///taplo-test/sub/inner.json#/definitions/port` | `/definitions/port` |
| `#` | `file:///taplo-test/sub/inner.json#` | empty |
| `#foo` | `file:///taplo-test/sub/inner.json#foo` | `foo` |
| `common.json#/definitions/port` | `file:///taplo-test/sub/common.json#/definitions/port` | `/definitions/port` |
| `../common.json` | `file:///taplo-test/common.json` | none |
| `/abs.json` | `file:///abs.json` | none |
| `https://example.com/x.json#/a` | `https://example.com/x.json#/a` | `/a` |
| `defs/port.json#port` | `file:///taplo-test/sub/defs/port.json#port` | `port` |

One call replaces the three-way special case, and the fragment it produces is uniform: the
fragment as written, leading `/` intact. So `resolve_schema` stops synthesizing one.

> A fragment is a JSON pointer when it starts with `/`, and a plain-name anchor otherwise.
> An absent or empty fragment names the whole document.

Percent-decoding is required, not optional: `Url::join` percent-*encodes* on the way in,
so `#/a b` comes back as the fragment `/a%20b` and `#/café` as `/caf%C3%A9`. `~0` and
`~1` survive the round trip untouched and need no handling of their own, because
`serde_json`'s `pointer` already unescapes them.

Three edges the join produces that the fragment rule has to name. An empty reference
`""` is a legal self-reference and joins to the base minus its fragment, query intact.
A query-only reference joins to a URL the cache is not keyed by, so it fetches; nothing
in the checklist writes one, and fetching is the honest answer for a URL naming a
different resource. And `#` joins to a URL whose fragment is `Some("")`, which is the
whole document — the case that errors today, because `resolve_schema` builds the pointer
`/` from it.

`Url::join` fails on a cannot-be-a-base URL. Every scheme that reaches `fetch_external`
— `http`, `https`, `file` — has an authority, and so does the one builtin,
`taplo://taplo.toml`. A join that fails is treated as an unresolvable reference, the
same as today.

`root_url` becomes `base_url` in both traversal functions, and moves on exactly two events:

1. **Entering a schema object that carries `$id`.** The new base is `base.join($id)` with
   the fragment dropped. A `$id` that fails to join leaves the base alone, because a base
   that cannot be built is worse than a stale one.
2. **Resolving a `$ref`.** The target's base is the joined URL with its fragment dropped,
   then rule 1 applied to whatever object the fragment landed on.

That is `find_schemas`'s rule in `jsonschema`, minus the pre-built index, and it is what
makes the probed inner-`$id` case work: `{"$id": "defs/", "$ref": "port.json"}` inside
`file:///taplo-test/schema.json` re-bases to `file:///taplo-test/defs/` and resolves
`port.json` to `file:///taplo-test/defs/port.json`.

### Two resolvers, one behavior

They cannot be one. `jsonschema::resolver::Resolver` is `pub(crate)` in 0.17.1, the crate
exports only the `SchemaResolver` trait a consumer implements, and the resolver is
constructed inside `CompilationOptions::compile` from a scope the caller cannot set — there
is no `with_base_uri`. Nothing about it is reachable from `taplo-common`, and making it
reachable means the upgrade to 0.55 that the draft-versions feature set already deferred to
its own feature set.

Traversal also asks a different question. `jsonschema` resolves a reference once, at compile
time, into a validator. Traversal resolves one per request, at a cursor position, and has to
keep walking afterwards. It needs the resolved `Value`, not a validator over it.

So: two implementations, one behavior, and the behavior pinned by a test that asserts
traversal and validation agree on the same fixture rather than by a shared function.
Bringing them into agreement needs one change on the validation side, because the scope
`create_validator` compiles under is wrong in the three ways probed under "Problem":

> `add_validator` knows the URL the schema was loaded from. Before compiling, it makes the
> root `$id` absolute against that URL — setting it when there is none, joining it when it
> is relative, leaving it alone when it is already absolute. For a root that compiles
> as draft 4 the keyword written is `id`, because that is the one `jsonschema` reads
> under that draft.

Probed: the schema with a relative reference and no `$id` goes from
``Err("the scheme `json-schema` is not supported")`` to
`Ok(["\"nope\" is not of type \"integer\""])`. It also repairs the relative-`$id` case,
where compilation fails outright today. The draft-4 spelling is not optional — probed
against the same fixture compiled as draft 4, writing `$id` leaves it erroring and
writing `id` fixes it, because `id_of` reads only `id` under that draft. `add_validator`
already knows the draft, through `declared_draft`.

The "`$id` only" non-goal is about what traversal *reads* from a document. What the
validator is *told* is a different question, and there the draft is known.

This is not the same code path as traversal's base, and it is not meant to be. It is the one
change that makes `jsonschema`'s own resolver start where traversal starts, after which the
two agree because both implement RFC 3986.

The cost is one clone of the root schema per validator built. The validator LRU holds three
and `add_validator` runs on a miss, so that is three clones per LRU generation, not one per
request.

### A plain-name fragment

`#port` names the subschema whose canonical URI ends in that fragment. `jsonschema` answers
it from an index `find_schemas` builds over the whole document at compile time. Traversal
answers it on demand, walking the target document once with the same rule:

```rust
/// The subschema in `document` whose `$id` or `$anchor` resolves to `url`.
///
/// Mirrors the index `jsonschema` builds at compile time: `$id` joins onto the
/// base in force and re-bases everything beneath it, `$anchor` names a fragment
/// on the base in force. `enum` and `const` are skipped, because their contents
/// are instance data and a key named `$id` inside one is a value, not an
/// identifier.
fn anchored_subschema<'d>(document: &'d Value, base: &Url, url: &Url) -> Option<&'d Value>
```

Walking on demand rather than indexing up front is the cheaper shape here: the walk happens
only when a fragment is a plain name, which is the rare reference, where an index would be
built for every document traversal touches.

`$anchor` is read beside `$id` for the reason `prefixItems` is read beside tuple `items`:
traversal is draft-agnostic, and reading whichever spelling a document carries costs nothing
and matches whichever draft it was written for.

### Keywords beside a `$ref`

At a `$ref` whose object carries other keys, the resolved target is merged **under** those
keys and traversal continues into the merged value. One schema comes out, not two.

Merging rather than yielding both is what keeps this from re-opening a problem the applicator
feature set measured. Yielding the carrier beside the target is what
`include_self = schema["allOf"].is_null()` was written to prevent, and lifting that exclusion
doubles every value completion under the carrier, because `possible_schemas_from` runs
`collect_child_schemas` on each of the two, they differ by a `description`, `unique_by` keeps
both, and `add_value_completions` deduplicates within one schema rather than across the set.
A merge produces one schema and the question does not arise.

The overlay is the referring object minus the keywords that identify it rather than
describe its instance: `$ref` itself, `$id`, `$anchor`, `$schema`, `$comment`,
`definitions` and `$defs`. Dropping `$ref` is what stops the merged value from resolving
itself again; dropping `$id` is what stops the carrier's identity from re-basing the
target's relative references to the carrier's directory; dropping the definition
containers is what keeps `{"$ref": "#/$defs/model", "$defs": {...}}` — the root shape
`pydantic` emits, which resolves correctly today — on the fast path instead of merging a
document into its own subschema.

So the fast path is "a `$ref` with no *applicable* sibling", and it is the common case:
no siblings, no clone, no merge.

The merge rule is written out rather than delegated:

```rust
/// `overlay`'s keys win over `base`'s, except where both hold an object that
/// describes a schema rather than an instance, which merge recursively.
///
/// `const`, `default`, `examples` and `enum` hold instance data, so an overlay
/// replaces them whole. Merging them would compose a value nobody wrote.
fn merged_over(base: &Value, overlay: &Value) -> Value
```

`json_value_merge`'s `Merge`, which `collect_child_schemas` uses for its composed-`allOf`
branch, is the wrong tool here. Probed: it concatenates arrays, so `{"enum": [1, 2]}`
merged with `{"enum": [3]}` gives `{"enum": [1, 2, 3]}` and `{"required": ["a"]}` merged
with itself gives `["a", "a"]`. A sibling `enum` beside a `$ref` would widen the target's
rather than replace it.

Replacing rather than intersecting is a choice, and the honest framing is that neither
answer is the validator's. A 2019-09 validator applies both, so the instances that pass
are the *intersection* — which for a sibling `enum` of `["a"]` over a target `enum` of
`["b", "c"]` is empty. Traversal cannot offer an empty completion list as an answer, and
the author who wrote the sibling meant it to narrow the target, so the sibling wins.
Objects recurse for the opposite reason: a sibling `properties` unions with the target's,
because a key described by either is a key the reader may write.

The overlay is rewritten through `absolute_refs` against the *carrier's* base before the
merge, and traversal continues into the merged value under the *target's* base. Without
that rewrite a sibling `{"properties": {"extra": {"$ref": "#/definitions/x"}}}` written in
the root document would resolve against the target's document instead of its own. The
same bug sits in today's composed-`allOf` merge, unreachable only because a relative
reference errored before it got there; the members it merges need the same rewrite.

Two merges now exist and they compose in one order: `merged_over` runs *inside* a member,
folding a `$ref`'s siblings into its target, and `json_value_merge`'s `Merge` runs
*across* members in the composed-`allOf` branch, as it already does. Nothing changes about
the second.

Traversal honors siblings under every draft, where `jsonschema` honors them only from
2019-09. That is deliberate, and it is the one place this feature set makes traversal more
generous than the validator. The generosity is confined to what traversal is for: a
`description` written beside a `$ref` in a draft-7 schema is documentation the author wrote
for a reader, and hiding it because a 2019 revision permits the validator to ignore it serves
nobody. Where the sibling is an applicator, the union traversal produces is a superset of the
branch, so the failure mode is offering a key that does not validate, which the applicator
feature set already argued is the recoverable direction. It is also what the VS Code JSON
language service does, so a schema author sees the same keys in both editors.

### A condition holding a nested reference

`names_a_ref` and the undecidable case it guards are removed. Before a condition compiles,
every `$ref` string inside it is rewritten to its absolute form against the base in force:

```rust
/// A copy of `schema` whose every `$ref` string is absolute against `base`.
///
/// A subschema compiled outside its document keeps its `#/...` pointers and
/// loses the document they name, so every reference fails and the subschema
/// rejects every instance. An absolute reference reaches the same document
/// through `CacheSchemaResolver`, which reads the cache traversal has already
/// populated.
///
/// A nested `$id` re-bases the references beneath it, as it does in traversal.
fn absolute_refs(schema: &Value, base: &Url) -> Value
```

The walk is over the condition's own JSON. It resolves nothing, fetches nothing and never
follows a reference, so it cannot cycle however cyclic the schema is. A `$ref` that fails to
join is left as written, which returns that one reference to today's behavior.

`condition_holds` keeps its `Option<bool>` and decides with `validate` rather than
`is_valid`. Compilation is not where a reference is resolved: `RefValidator::compile` only
builds the URL, and resolution happens on first evaluation. So a condition naming a
document the cache does not hold compiles cleanly and `is_valid` reports `false` — the
silent `else` that `names_a_ref` existed to prevent, reintroduced through the back door.
Probed on a condition seeded against a document that exists and one that does not:

| Reference inside the condition | `is_valid` | `validate` |
|---|---|---|
| `…/schema.json#/definitions/docker` | `true` | no errors |
| `…/missing.json#/definitions/docker` | `false` | `Resolver`: failed to resolve |
| `…/schema.json#/definitions/nope` | `false` | `InvalidReference` |

So the rule is: an error of kind `Resolver` or `InvalidReference` among the validation
errors means the condition could not be resolved and returns `None`; no errors means it
holds; anything else means it does not. That is the insurance, and it is stronger than the
refusal it replaces, because it distinguishes "false" from "unanswerable" where
`names_a_ref` could only guess from the shape.

`a_condition_holding_a_nested_reference_takes_both_branches` is rewritten to assert that
the condition now decides.

### Bounding work, not just depth

`MAX_COMPOSITION_DEPTH` stays, and a set of resolved `$ref` URLs is carried beside it:

> A traversal carries the set of `$ref` URLs it has resolved since the last descent that
> consumed a path segment. A `$ref` whose URL is already in the set is not followed. Every
> in-place descent passes the set on; every descent that consumes a path segment starts an
> empty one, exactly where `MAX_COMPOSITION_DEPTH` already resets.

It is a `Vec<Url>` passed by mutable reference: a `$ref` pushes its URL before descending
into the target and pops it after, so sibling branches never see each other's entries and
nothing is cloned. Branches run sequentially — every composition loop `await`s one member
before starting the next — so push-and-pop gives exactly the per-chain semantics the rule
describes. A `Vec` rather than a hash set, because the chains it bounds are at most
`MAX_COMPOSITION_DEPTH` long and a linear scan of thirty-two URLs is cheaper than hashing
them.

The composed-`allOf` branch in `collect_child_schemas` consults the same set, and a member
whose URL is already in it is left out of the merge. It has to: that branch resolves its
members through `ref_schema_value` and merges them rather than recursing on `$ref`, so no
early return ever consults anything, and `json_value_merge` concatenates arrays, which
doubles the merged `allOf` on every round. Probed through `possible_schemas_from` at the
root, on `node = {"allOf": [{"$ref": m0}, …]}` where each member points back at `node`:

| Fan-out | Time |
|---|---|
| one | 853 µs |
| two | 1.44 s |
| three | did not finish in 400 s |

This turns both cycles from exponential into linear, and leaves `MAX_COMPOSITION_DEPTH` as
the backstop rather than the mechanism: a cycle stops at its second visit, and the budget is
left to bound composition chains that make progress through distinct references.

### What `$defs` needs

Nothing. Traversal applies the fragment as a JSON pointer to the raw document, so
`#/$defs/port` finds `$defs` for the reason `#/definitions/port` finds `definitions`: it is a
key in an object, and no draft is consulted. Probed under a 2020-12 root, and the pointer path
is draft-independent by construction. The checklist item is closed by tests that pin it under
each draft the validator supports, so a later change to fragment handling cannot quietly break
it.

## Behavior changes to accept

**A schema that failed to load now loads.** Every row of the end-to-end table reading `Err`
becomes a resolved schema, which is the point. A user whose completion was empty gets
completion; a user whose document had no diagnostics at all, because `validate` errored on a
relative reference, gets diagnostics — including ones they have never seen for a document they
believed was clean. Today's failure is silent in both directions: `validate` errors, the
diagnostics handler logs `schema validation failed`, and the editor shows a clean document.

**Hover text gains sibling annotations.** A key written as `{"$ref": ..., "description": ...}`
shows the sibling's description where it showed the target's. That changes text for every
schema using the pattern, `schemars` output included.

**A conditional whose `if` holds a reference picks one branch.** It showed both. Where the
document decides the condition, the other branch's keys stop being offered and its hover block
stops rendering. This is the case the applicator feature set deliberately over-offered, and
narrowing it is what the tracking issue asks for.

**Traversal reaches documents it did not reach before, which means requests it did not make
before.** A relative reference under an `https` base is an HTTP request on a code path that
previously errored out. The concurrency semaphore, the cache and the expiry are unchanged, so
this is more of what already happens rather than a new kind of it.

**`additionalProperties: false` beside a `$ref` still offers the target's keys.** Under
2019-09 the validator applies both and rejects every key, because the carrier evaluates none
of its own. Traversal offers what the target describes. This is the shape of the "offering a
key that does not validate" trade named above, and it is the shape people actually write, so
it is named rather than left to the general argument.

**The same reference reached twice on one chain with different siblings loses the second
carrier's keywords.** The visited set is keyed on the resolved URL. For a bare `$ref` that is
lossless — same schema, same instance, same path, so the second visit would push a duplicate
`unique_by` drops anyway. With siblings it is not, and no realistic schema writes it, so the
set stays keyed on the URL rather than on the carrier.

**A reference cycle is followed once rather than sixteen times.** A schema that reaches a
position only by going around a cycle twice — through the same `$ref` URL twice with no
property descent between — stops resolving. No such schema is known; the shapes that reach the
limit today are the ones that hang.

**`possible_schemas_from` still swallows a resolution failure and `schemas_at_path` still
errors on one.** The asymmetry is inherited and untouched. Fewer references fail, so it shows
less often, but a reference to a document that genuinely does not exist still costs the whole
`schemas_at_path` call.

## Deferred

**The `allOf` carrier that `collect_schemas` discards.** `include_self =
schema["allOf"].is_null()` drops a carrier's own `description` whenever it also writes `allOf`,
which is what `schemars` emits for every documented non-`Option` field. The applicator feature
set probed it and deferred it here on the grounds that lifting the exclusion doubles value
completion.

The merge this feature set builds dissolves that objection — a carrier merged over its members
yields one schema, not two — but the item stays deferred, for a different reason. It is not a
reference problem. The keyword is `allOf`, the carrier's member is a `$ref` only by convention,
and the fix is to give `collect_schemas` the composed-`allOf` merge `collect_child_schemas`
already has. That merge is `json_value_merge`'s, the one probed above as concatenating arrays,
so extending it means first settling what it does to the `enum` and `required` of every `allOf`
carrier in every schema taplo validates against. The blast radius is every hover over an
`allOf` carrier, which is most hovers over a `schemars` schema, and none of it shares a line
with reference resolution.

It should be its own feature set, and since this is the last one in the stack, it should be a
line on the tracking issue: *`collect_schemas` discards an `allOf` carrier's own annotations;
give it the composed-`allOf` merge `collect_child_schemas` has, after settling what that merge
does to arrays.*

**A `$ref` to an embedded resource.** A subschema whose `$id` gives it its own URL inside a
larger document, referenced by that URL, resolves in validation through `jsonschema`'s index
and will not resolve in traversal: `resolve_schema` drops the fragment, joins to the URL, and
fetches a document that does not exist on disk. Probed on `{"$id": "nested.json"}` inside the
root, referenced as `{"$ref": "nested.json"}` — validation reports the expected type error,
traversal errors today and will attempt a fetch afterwards. `anchored_subschema` is almost the
walk that would find it, matched on the fragment-less URL instead of the fragment, so closing
it means consulting the current document before `load_schema` for every fragment-less join.
That is a change to what "load a schema" means rather than to what a reference resolves to,
and it belongs with the fetch layer. It is a line for the tracking issue.

**A document whose root `$id` disagrees with the URL it was fetched from.** The specification
says the `$id` wins for everything inside it. Traversal will use it, through rule 1; validation
will too, through the absolutization in `add_validator`, which leaves an absolute `$id` alone.
Where they will differ is the cache key, which stays the fetch URL in both. Nothing in the
checklist depends on it and no probe produced a case where it matters, so it is named rather
than handled.

## Acceptance criteria

Traversal criteria run against `schemas_at_path` and `possible_schemas_from` in
`crates/taplo-common/src/schema/tests.rs`. Cross-document criteria need more than one seeded
document, so `seeded` gains a sibling:

```rust
/// Seeds several documents, the first of which is the root, and returns its URL.
async fn seeded_documents(documents: &[(&str, Value)]) -> (Schemas<NativeEnvironment>, Url)
```

Rendering criteria run against the real `hover` and `completion` handlers through
`hover_at_line` and `complete_at_line` in `crates/taplo-lsp/src/handlers/hover.rs`'s
`mod tests`, whose `world_with` gains the same treatment:

```rust
/// Builds a world holding one document and several schemas, the first of which
/// is associated with the document.
async fn world_with_documents(
    schemas: &[(&str, serde_json::Value)],
    source: &str,
) -> (Arc<WorldState<NativeEnvironment>>, Url)
```

1. `schemas_at_path` resolves `common.json#/definitions/port` against a root loaded from
   `file:///taplo-test/schema.json`, returning the target's schema.
2. It resolves `../shared/common.json#/definitions/port` and `/abs/common.json` by the same
   rule, and an `https` base joins a relative reference to an `https` URL.
3. It resolves `file:///taplo-test/common.json#/definitions/port`, an absolute URL carrying a
   JSON pointer, which errors today.
4. It resolves `$ref: "#"` to the whole root document.
5. A fragment containing a percent-encoded character resolves to the key that character
   spells, and a pointer containing `~1` resolves to the key containing `/`.
6. A subschema carrying `{"$id": "defs/"}` re-bases the references written inside it, so
   `{"$id": "defs/", "$ref": "port.json"}` inside `file:///taplo-test/schema.json` resolves to
   `file:///taplo-test/defs/port.json`.
7. A reference across two documents re-bases at the target: a relative reference written inside
   `sub/inner.json` resolves against `sub/`, not against the root document's directory.
8. `#port` resolves to the subschema carrying `{"$id": "#port"}`, and to the subschema carrying
   `{"$anchor": "port"}`.
9. `#/$defs/port` resolves under a root declaring draft 7, one declaring 2019-09 and one
   declaring 2020-12, and so does `#/definitions/port`.
10. `{"$ref": ..., "description": ...}` yields one schema at that path, carrying the sibling's
    description and the target's `type`.
11. `{"$ref": ..., "enum": ["a"]}` over a target whose `enum` is `["b", "c"]` yields `["a"]`
    and not `["a", "b", "c"]`.
12. `{"$ref": ..., "properties": {"extra": ...}}` yields a schema at `extra` and at every key
    the target's own `properties` names, and a sibling `$ref` written inside that `properties`
    resolves against the document the sibling was written in.
12b. A root written as `{"$ref": "#/$defs/model", "$defs": {...}}` — `$defs` beside a `$ref`,
    which is not an applicable sibling — resolves exactly as it does today, on the fast path.
13. `{"$ref": ..., "unevaluatedProperties": {...}}` over a target that does not evaluate a key
    yields the `unevaluatedProperties` schema for it, closing the divergence the applicator
    spec recorded.
14. Hover through the real handler renders the sibling description of a key written as
    `{"$ref": ..., "description": ...}`.
15. Completion through the real handler offers the keys of a target reached by a relative
    reference across documents.
16. A condition holding a nested `$ref` decides its branch: `{"kind": "docker"}` selects
    `then` and `{"kind": "podman"}` selects `else`, where both branches are returned today.
    `a_condition_holding_a_nested_reference_takes_both_branches` is rewritten to assert it.
17. A condition whose nested `$ref` names a document the cache does not hold
    (`file:///taplo-test/missing.json#/x`) returns both branches, and so does one naming a
    pointer that does not exist. Asserted through `schemas_at_path`, so that deciding by
    `is_valid` — which reports plain `false` for both — cannot make it pass.
18. `validate` reports the expected diagnostic for a schema with a relative reference and no
    `$id`, where it errors today; for a schema whose root `$id` is relative, where compilation
    fails today; and for a draft-4 root with a relative reference and no `id`.
19. Traversal and validation agree on every shape in the end-to-end table: each resolves,
    asserted in one test that runs both against the same seeded documents.
20. `schemas_at_path` over an `anyOf` of three references back to itself finishes in under a
    second, against 63 s today, and the two-reference case in under 100 ms, against 531 ms.
    `possible_schemas_from` over a composed `allOf` of two references back to itself finishes
    in under 100 ms, against 1.44 s today.
21. Every schema `schemas_at_path` and `possible_schemas_from` returned before this feature set
    is still returned: every test in
    `cargo test -p taplo-common --features schema,reqwest,rustls-tls` and in
    `cargo test -p taplo-lsp --lib handlers::hover` passes, none removed and none weakened.
22. `cargo check --workspace --all-targets`, `cargo test --workspace` and
    `cargo check --target wasm32-unknown-unknown --manifest-path crates/taplo-wasm/Cargo.toml`
    are clean.

## Landing

Seven commits on `feat/schema-ref-resolution`, each building and passing on its own. This is
the last branch in a stack of five, so these are commits rather than separate pull requests;
the branch opens one pull request.

1. `perf(schema): stop revisiting a ref in one chain` — the visited set, in both traversals
   and in the composed-`allOf` merge. First, because it is the only change here that fixes a
   hang that exists today, and because every commit after it adds tests over cyclic fixtures.
   It stores whatever URL the resolver produces, so the swap in commit 2 costs it nothing.
   Criterion 20.
2. `fix(schema): resolve a ref as a uri reference` — `reference_url` deleted in favor of
   `Url::join`, `resolve_schema`'s fragment read as written and percent-decoded, and the
   `$defs` confirmation tests. Fixes relative references at one hop, absolute references
   carrying a pointer, and `$ref: "#"`. Criteria 1 through 5, and 9.
3. `feat(schema): carry a base url through traversal` — `root_url` becomes `base_url`, moved
   by `$id` and by each resolved target. Criteria 6 and 7.
4. `feat(schema): resolve a plain-name anchor` — `anchored_subschema`. Criterion 8.
5. `feat(schema): apply keywords written beside a ref` — `merged_over`, `absolute_refs`, and
   the sibling path in both traversals. Criteria 10 through 15.
6. `feat(schema): decide a condition holding a ref` — `absolute_refs` reused, `names_a_ref`
   removed, the condition decided through `validate`. Criteria 16 and 17.
7. `fix(schema): validate against the schema's own url` — the `$id`/`id` absolutization in
   `add_validator`, and the agreement test. Criteria 18 and 19.

The split is by mechanism, and each is a thing a reviewer could reject without rejecting its
neighbors. `absolute_refs` lands in commit 5 rather than in a commit of its own, because that
is the first commit with a caller for it; commit 6 is its second. Commit 7 touches only the
validator.

## Reproducing the findings

Every table came from `#[tokio::test]`s appended to
`crates/taplo-common/src/schema/tests.rs`, reusing the `seeded` and `descriptions` helpers
already there, formatting into a string and ending in `panic!` so the output survives. Run with
`cargo test -p taplo-common --features schema,reqwest,rustls-tls <name> -- --nocapture --test-threads=1`.

The cross-document rows seeded each document into the cache directly with
`schemas.cache().store(url, Arc::new(value))`, which is what `seeded` does for one.

The timings came from `std::time::Instant` around a single call on a debug build, so they are
worst-case rather than representative; the ratio between them is the finding, not the absolute
number.

The `json_value_merge` row came from calling `Merge::merge` on two `Value`s directly.

The `jsonschema` claims — the `pub(crate)` resolver, the compile scope, the sibling check,
`id_of` — came from reading `jsonschema` 0.17.1's source under
`~/.cargo/registry/src/*/jsonschema-0.17.1/src/`, and each was then confirmed by a probe through
`validate`.

Every probe was reverted; the baseline at `dc31977` — the tree this branch starts from, and
the tree at `9766900` minus this document — is unchanged and green:
`cargo check --workspace --all-targets`, `cargo test --workspace`,
`cargo test -p taplo-common --features schema,reqwest,rustls-tls` (39 tests),
`cargo test -p taplo-lsp --lib handlers::hover` (57 tests) and the wasm check all pass.

Verified against `jsonschema` 0.17.1 at Taplo commit `9766900`.

## Questions settled

Each was open when this spec was first written, and each was answered by probing rather
than by argument. The answers are folded into the sections above; what follows is the
decision and the reason, so that a reader who disagrees knows what to reopen.

1. **The visited set lands first, not last.** It stores whatever URL the resolver hands it,
   and a cycle through `#/definitions/...` is a cycle under the resolution that exists today,
   so nothing about it is revised when the join changes. Landing it last would carry a
   63-second hang through six commits and run every later commit's cyclic fixtures against an
   unbounded traversal.

2. **A condition's references are absolutized, and `names_a_ref` is removed.** The insurance
   is not a failed compile — a reference resolves lazily, so a missing document compiles
   cleanly and reports plain `false`. The insurance is `validate` rather than `is_valid`,
   reading `Resolver` and `InvalidReference` as "unanswerable".

3. **Traversal honors `$ref` siblings under every draft.** The precedent is settled — it reads
   `prefixItems` beside tuple `items` without a draft — and the VS Code JSON language service
   does the same, so a schema author sees the same keys in both editors. Limiting the merge to
   the rendering keywords would drop criteria 12 and 13, and 13 is what closes the divergence
   the applicator spec recorded.

4. **`add_validator`'s absolutization stays in this feature set.** Without it, agreement
   between the two halves is unreachable and the tracking issue's framing stays true. It is
   not a surprise: today the failure is a `validate` error that the diagnostics handler logs
   and the editor renders as a clean document, so the change is from silently wrong to
   correct.

5. **`anchored_subschema` stays.** Dropping it would not leave anchors unsupported; it would
   leave them erroring, which costs every path beneath such a reference its hover and its
   completion. What it does *not* cover — a `$ref` to an embedded resource — is written down
   under "Deferred" instead.
