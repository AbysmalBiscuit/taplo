# JSON Schema draft version support

**Status:** Draft
**Tracking issue:** AbysmalBiscuit/taplo#1, "Draft versions" section

## Problem

Taplo validates every schema as draft 7 regardless of what the schema declares. A schema that says `"$schema": "https://json-schema.org/draft/2020-12/schema"` is silently downgraded. No error, no warning, no log line. Keywords that exist only in 2019-09 and 2020-12 are ignored, so a document that violates them passes validation clean.

2020-12 is what most new schemas declare, so this is not an edge case.

Three separate defects produce the single symptom. Fixing any one alone changes nothing user-visible.

### Defect 1: the draft features are off

`crates/taplo-common/Cargo.toml` declares:

```toml
jsonschema = { version = "0.17.1", default-features = false }
```

Neither `draft201909` nor `draft202012` is enabled, so those `Draft` enum variants are removed by `#[cfg]` and the keywords gated on them compile to nothing.

Both features are defined as `draft201909 = []` and `draft202012 = []` in the crate's own manifest. They pull no additional dependencies. The cost is compile time and binary size, nothing else.

### Defect 2: draft detection requires a trailing `#`

`jsonschema::schemas::draft_from_url` matches the `$schema` value as an exact string:

```rust
"https://json-schema.org/draft/2020-12/schema#" => Some(Draft::Draft202012),
```

The canonical `$schema` value in the 2020-12 specification carries no fragment. Real schemas write it without the `#`, so detection returns `None` and `Draft::default()` (draft 7) takes over.

This is the defect that makes the others invisible. Enabling the cargo features alone does not fix it.

Verified by A/B probe with both features enabled:

| `$schema` value | Errors on `{"a": "x", "b": 1}` against `unevaluatedProperties: false` |
|---|---|
| `https://json-schema.org/draft/2020-12/schema` | 0 |
| `https://json-schema.org/draft/2020-12/schema#` | 1 |

### Defect 3: traversal does not know `prefixItems`

`collect_schemas` in `crates/taplo-common/src/schema/mod.rs` handles array positions by reading `items`, in both the single-schema and the draft-7 tuple form. It never reads `prefixItems`.

Validation and traversal are independent code paths, so enabling the cargo features fixes diagnostics inside a 2020-12 tuple array while completion and hover stay blank there.

Verified by probing `schemas_at_path` at index 0 of three array shapes:

| Schema shape | Schemas found at index 0 |
|---|---|
| `"prefixItems": [{...}]` | 0 |
| `"items": [{...}]` (draft-7 tuple) | 1 |
| `"items": {...}` (single schema) | 1 |

## Goals

- A schema declaring 2019-09 or 2020-12 validates against that draft's rules.
- A schema declaring a draft Taplo cannot honor produces a visible diagnostic rather than a silent downgrade.
- Completion and hover work at every position inside a `prefixItems` array.
- Draft 4, 6 and 7 schemas behave exactly as they do today.

## Non-goals

Upgrading `jsonschema` past 0.17.1. That is a separate piece of work with its own spec, and this one is written to be shippable without it. See "Deferred" below for what the upgrade would unlock.

Rendering any of the newly validated keywords in hover text. That is the "Constraints in hover" section of the tracking issue.

## Design

### Detect the draft in Taplo, not in the dependency

`create_validator` in `crates/taplo-common/src/schema/mod.rs` already builds through `JSONSchema::options()`. The options builder exposes `with_draft`, and an explicitly set draft takes precedence over the dependency's own `$schema` sniffing, as documented in its `compile` method:

```
// Draft is detected in the following precedence order:
//   - Explicitly specified;
//   - $schema field in the document;
//   - Draft::default()
```

So Taplo reads `$schema` itself, normalizes away the fragment, maps the result to a `Draft`, and passes it explicitly. This routes around defect 2 without patching or forking the dependency.

Normalization strips one trailing `#`, and nothing else. A `$schema` value that differs from a known meta-schema URI in any other way stays unrecognized, which is the correct outcome.

### Report an unhonored draft instead of downgrading

A `$schema` naming a draft Taplo has no support for currently falls back to draft 7 in silence. It should surface. The existing `validate` path already returns errors that reach diagnostics, so the fallback becomes a warning the user can see.

The distinction that matters: an *unrecognized* `$schema` (a typo, a private meta-schema) is different from a *recognized but unsupported* one. Only the second warrants a warning, since the first may be intentional.

### Teach traversal about `prefixItems`

`collect_schemas` gains a `prefixItems` branch for `KeyOrIndex::Index`, ordered so that `prefixItems[idx]` is consulted before `items`. Under 2020-12 the two compose: `prefixItems` covers the leading positions and `items` covers the rest. Traversal collects from both rather than choosing one, matching how it already collects from `properties`, `additionalProperties` and `patternProperties` at the same position.

The existing draft-7 tuple branch stays. Taplo supports draft 4 through 2020-12 simultaneously and the traversal code is deliberately draft-agnostic, reading whichever keywords a schema happens to carry.

## Behavior changes to accept

**Enabling the features changes draft-7 validation.** `prefixItems`, `dependentRequired` and `dependentSchemas` are wired in `jsonschema`'s keyword table as flat match arms gated only on the cargo feature, not on the detected draft value. Once compiled in, they apply under every draft. A draft-7 schema carrying a `prefixItems` key should ignore it per spec, and after this change Taplo will validate against it.

This is a conformance deviation. It is also unlikely to bite anyone, since a schema author who writes `prefixItems` means it. Documented here so it is a decision rather than a surprise.

**More keys gain hover popups and more documents gain diagnostics.** Both are the point of the change, but they will read as new noise to anyone whose schema was quietly half-validated before.

## Deferred

Three items cannot be fixed on `jsonschema` 0.17.1 and are out of scope:

- `unevaluatedItems` has no implementation in the crate at all. Only `unevaluated_properties.rs` exists.
- `$dynamicRef` and `$dynamicAnchor` are unimplemented, so a schema using them validates as though the reference were absent.
- `$recursiveRef` likewise.

The crate is at 0.55 upstream. That upgrade renames the core type, replaces the `SchemaResolver` trait that `CacheSchemaResolver` implements, and reshapes the error types, so it needs its own spec.

## Acceptance criteria

1. A schema declaring `https://json-schema.org/draft/2020-12/schema`, with and without a trailing `#`, validates `unevaluatedProperties` correctly.
2. The same holds for `https://json-schema.org/draft/2019-09/schema`.
3. A schema declaring a recognized but unsupported draft produces a warning naming the declared draft and the one used instead.
4. A schema with no `$schema` still validates as draft 7.
5. `schemas_at_path` returns the correct schema at every index of a `prefixItems` array, including indices past the end of `prefixItems` when `items` is also present.
6. Existing draft-7 traversal for both `items` forms is unchanged.
7. `cargo test -p taplo` and `cargo test -p taplo-common --features schema,reqwest,rustls-tls` pass.

## Reproducing the findings

Every claim above came from probes against the real validator. To re-derive them, enable the features:

```toml
jsonschema = { version = "0.17.1", default-features = false, features = ["draft201909", "draft202012"] }
```

then write a `#[tokio::test]` in `crates/taplo-common/src/schema/tests.rs` that seeds a schema through the existing `seeded()` helper and calls `schemas.validate(&url, &instance)`, printing `errors.len()`. Compare against the same test with the features removed. Traversal claims use `schemas.schemas_at_path(&url, &value, &keys)` with keys built as `"name".parse::<Keys>().unwrap().join(0_usize)`, since `Keys::from_str` parses TOML keys and will read a bare `0` as a key named "0" rather than an index.

Verified against `jsonschema` 0.17.1 at Taplo commit `93f6b4f`.

## Open questions

1. Should the unsupported-draft warning be a diagnostic on the TOML document, a tracing log line, or both? A diagnostic is visible but blames the wrong file, since the fault lies in the schema.
2. Is the draft-7 conformance deviation acceptable, or should Taplo gate `prefixItems`, `dependentRequired` and `dependentSchemas` on the detected draft itself to compensate?
3. Should this land as one PR or two, splitting validation from traversal? They are independent and each is separately testable.
4. Does upstream Taplo want this? If a PR to `tamasfe/taplo` is the goal, the conformance deviation in particular is worth raising with them first.
