# JSON Schema draft version support

**Status:** Accepted
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

The same exact-match rule applies to the draft-04, draft-06 and draft-07 URIs, whose canonical form does carry the `#`. A pre-2019 schema that omits it is also mis-detected, but falls back to draft 7 so only draft-04 and draft-06 schemas are affected.

This is the defect that makes the others invisible. Enabling the cargo features alone does not fix it. It does, however, change behavior on its own: a schema that writes the 2020-12 URI *with* the `#` starts compiling as 2020-12 the moment the features are on, which is why the format-validation guard below has to land in the same commit.

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
- A schema declaring a draft Taplo cannot honor produces a visible warning rather than a silent downgrade.
- Completion and hover work at every position inside a `prefixItems` array.
- Draft 4, 6 and 7 schemas that declare `$schema` in the form `jsonschema` already recognizes (with the trailing `#`) behave exactly as they do today. Ones that omit the `#` run as draft 7 today and will run as declared; see "Behavior changes to accept".
- `format` keeps asserting under every draft, as it does today.

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

### Keep format validation on

`Draft::validate_formats_by_default` in `jsonschema` returns `false` for 2019-09 and 2020-12, and `format::compile` returns `None` when formats are not validated, custom formats included. Passing `with_draft(Draft202012)` therefore switches `format` off for those schemas, including the `semver` and `semver-requirement` formats `create_validator` registers, and every schema that validates formats today only because it fell back to draft 7 would stop.

`create_validator` calls `should_validate_formats(true)` so `format` keeps asserting under every draft. This is a deliberate departure from the 2020-12 default, where `format` is an annotation. Taplo registers custom formats explicitly, so treating `format` as assertive is the behavior its users already have and the one they asked for.

### Log an unhonored draft instead of downgrading silently

A `$schema` naming a draft Taplo has no support for falls back to draft 7 in silence. It should surface, as a `tracing::warn!` event, not as a document diagnostic.

Detection lives in `create_validator`, which maps `$schema` to the value it passes to `with_draft`:

```rust
enum DeclaredDraft {
    Supported(jsonschema::Draft),
    Unsupported(&'static str),
    Unrecognized,
}

fn declared_draft(schema: &Value) -> DeclaredDraft
```

`Supported` covers the five meta-schema URIs the crate has a `Draft` variant for (draft-04, draft-06, draft-07, 2019-09, 2020-12), with one trailing `#` stripped first. `Unsupported` covers meta-schema URIs that are recognizable but have no variant; with both features enabled that is draft-03. `Unrecognized` is everything else: a typo, a private meta-schema, no `$schema` at all. Only `Unsupported` logs, since `Unrecognized` may be intentional. Both fall back to `Draft::default()`.

The event is emitted once per validator compile. `create_validator` runs inside the `validate` span, which already carries `schema_url` as a field, so the log line names the schema without any signature change:

```rust
tracing::warn!(
    declared,
    used = "draft-07",
    "schema declares a draft taplo cannot validate, validating as draft-07 instead"
);
```

Why a log line and not a diagnostic. Every result of `validate_root` is a `NodeValidationError` and every consumer treats the vector as errors: `collect_schema_errors` in `crates/taplo-lsp/src/diagnostics.rs` publishes each as `DiagnosticSeverity::ERROR`; `lint_source` in `crates/taplo-cli/src/commands/lint.rs` fails the file whenever the vector is non-empty; `lint` in `crates/taplo-wasm/src/lib.rs` returns it as `LintResult.errors`. There is no warning channel. Adding one means a new return type threaded through three crates, a text range to anchor the warning on (the `#:schema` directive comment, or the start of the document when the association came from config), and a codespan `Diagnostic::warning` in `printing.rs`, all to warn about draft-03 schemas. The log line already reaches users: `setup_stderr_logging` filters at `INFO` when `RUST_LOG` is unset, so `taplo lint` prints it on stderr by default, and the VS Code extension shows the language server's stderr in its output channel.

If a per-document warning is ever wanted, the hook is a `SchemaWarning` returned alongside the errors from `validate_root`; nothing in this design blocks that.

### Teach traversal about `prefixItems`

`collect_schemas` gains a `prefixItems` branch for `KeyOrIndex::Index`. Under 2020-12 the two keywords partition the array: `prefixItems[idx]` applies when `idx < prefixItems.len()`, and single-schema `items` applies only to the positions past that. Traversal follows the same partition: when `prefixItems` is an array that covers `idx`, it descends into `prefixItems[idx]` and not into `items`; otherwise it descends into `items` exactly as today. Collecting both at a covered index would offer the tail schema's completions in a head position.

The existing draft-7 tuple branch stays. Taplo supports draft 4 through 2020-12 simultaneously and the traversal code is deliberately draft-agnostic, reading whichever keywords a schema happens to carry.

## Behavior changes to accept

**Enabling the features changes draft-4/6/7 validation.** In `jsonschema`'s keyword table (`Draft::get_validator` in `src/schemas.rs`) `prefixItems`, `dependentRequired` and `dependentSchemas` are flat match arms gated only on the cargo feature, unlike `unevaluatedProperties`, which is matched per draft and falls to `None` below 2019-09. Once compiled in, the three apply under every draft, and `items::compile` skips `prefixItems.len()` leading positions whenever a sibling `prefixItems` exists, again regardless of draft. A draft-7 schema carrying `prefixItems` should ignore it per spec; after this change Taplo validates it with 2020-12 array semantics.

This is a conformance deviation and it is accepted. It only adds constraints the author wrote, it is internally consistent (`items` and `prefixItems` compose the 2020-12 way wherever `prefixItems` appears), and the only shape where it yields a wrong verdict is a draft-7 schema that puts `prefixItems` next to tuple-form `items` or `additionalItems`, which nobody writes. Compensating in Taplo is not possible without a fork: `get_validator` is `pub(crate)` and `CompilationOptions` has no keyword hook, so the only route is stripping the three keys from the schema tree before `compile` and again from every document returned by `CacheSchemaResolver`, with draft-keyed copies because the cache is shared with traversal. Two recursive walks and a second cache to protect a case with no known victim. The deferred `jsonschema` upgrade is where this belongs; confirm per-draft keyword gating when that spec is written.

**Keywords next to `$ref` start validating under 2019-09 and 2020-12.** `compile_validators` in `jsonschema` isolates `$ref` and drops its siblings unless `supports_adjacent_validation(draft)` holds, which it does only for the two new drafts. A 2020-12 schema written as `{"$ref": "...", "type": "string"}` validates the reference alone today; after this change both apply. Correct per spec, and a source of new diagnostics. Traversal in `collect_schemas` still returns at `$ref` without reading siblings; that gap is unchanged and belongs with the `$ref` resolution work in the tracking issue.

**Draft-4 and draft-6 schemas that omit the trailing `#` change behavior.** `draft_from_url` demands the `#` for every draft, not only 2020-12, so a schema declaring `http://json-schema.org/draft-04/schema` runs as draft 7 today and will run as draft 4 after normalization: `id` instead of `$id` for scoping, boolean `exclusiveMaximum`, draft-4 `type` rules, no `const`, `contains`, `propertyNames` or `if`, and meta-schema validation (`validate_schema` defaults to on) against the draft-4 meta-schema instead of draft-7's. A schema that declares draft-04 but uses draft-6 constructs compiles today and will be rejected as an invalid schema. Exposure is small, since the fragment-less form is canonical only from 2019-09 onward. Normalizing only the 2019-09 and 2020-12 URIs would avoid this but would keep the same silent mis-validation this spec exists to remove; one rule for every draft wins.

**More keys gain hover popups and more documents gain diagnostics.** Both are the point of the change, but they will read as new noise to anyone whose schema was quietly half-validated before.

## Deferred

Three items cannot be fixed on `jsonschema` 0.17.1 and are out of scope:

- `unevaluatedItems` has no implementation in the crate at all. Only `unevaluated_properties.rs` exists.
- `$dynamicRef` and `$dynamicAnchor` are unimplemented, so a schema using them validates as though the reference were absent.
- `$recursiveRef` likewise.

The crate is at 0.55 upstream. That upgrade renames the core type, replaces the `SchemaResolver` trait that `CacheSchemaResolver` implements, and reshapes the error types, so it needs its own spec.

## Landing

This branch carries the spec and two code commits. Each code commit builds and passes `cargo test -p taplo` and `cargo test -p taplo-common --features schema,reqwest,rustls-tls` on its own.

1. `feat(schema): resolve prefixItems during schema traversal`. Touches `collect_schemas` and `tests.rs` only. Draft-agnostic, no dependency change, no user-visible behavior beyond hover and completion inside 2020-12 tuple arrays. Covers acceptance criteria 5 and 6.
2. `feat(schema): validate against the declared draft`. Enables the two cargo features, adds `declared_draft` with its warn arm, passes `with_draft` and `should_validate_formats(true)` in `create_validator`, and adds the validation tests. Covers the remaining criteria.

The dependency change is not its own commit. Enabling the features alone already routes every `#`-suffixed 2020-12 `$schema` to `Draft202012`, which turns format validation off and adjacent-`$ref` validation on for those schemas; a commit that does that without the `should_validate_formats(true)` guard and without tests is a regression point in bisect, not a logical unit. The warn log is not its own commit either: it is one arm of `declared_draft`.

Traversal lands first because it is independent and small; the validation commit is the one review is likelier to reshape, and a rework is cheaper at the tip.

## Upstream

Upstream `tamasfe/taplo` is in caretaker mode: the maintainer stepped back in December 2024 (issue #715), the planned move to an organization has not happened, the last merged PR is from March 2026, the last release (0.10.0) is from May 2025, and around thirty PRs are open. Issue #497 (October 2023) reports exactly this bug against a 2020-12 `unevaluatedProperties` schema; PR #498, which enabled the cargo features, was closed unmerged in September 2024 on the assumption that a `jsonschema` upgrade would make it moot, and the upgrade never happened. That PR would not have fixed the bug anyway: it lacked the fragment normalization.

The target of this work is `AbysmalBiscuit/taplo`. The upstream action is one comment on #497 after this lands: the root cause (features off, `draft_from_url` requiring a trailing `#`), a link to the fork commit, and a sentence on the draft-7 deviation. No new issue and no PR; nobody upstream is positioned to answer, and the deviation ends with the `jsonschema` upgrade anyway. A PR becomes worth opening only if the organization hand-off in #715 completes.

## Acceptance criteria

1. A schema declaring `https://json-schema.org/draft/2020-12/schema`, with and without a trailing `#`, validates `unevaluatedProperties` correctly.
2. The same holds for `https://json-schema.org/draft/2019-09/schema`.
3. A schema declaring a recognized but unsupported draft (draft-03) logs a `WARN` event naming the declared draft and the one used instead, validates as draft 7, attaches no diagnostic to the document, and leaves the `taplo lint` exit code unchanged. `declared_draft` has unit tests for all three variants; the log event itself is not asserted.
4. A schema with no `$schema` still validates as draft 7.
5. `schemas_at_path` returns the correct schema at every index of a `prefixItems` array, including indices past the end of `prefixItems` when `items` is also present, and does not return `items` at an index that `prefixItems` covers.
6. Existing draft-7 traversal for both `items` forms is unchanged.
7. `cargo check --workspace --all-targets`, `cargo test --workspace`, `cargo test -p taplo-common --features schema,reqwest,rustls-tls` and `cargo check --target wasm32-unknown-unknown` all pass.
8. A schema declaring 2020-12 with `"format": "semver"` still rejects a non-semver string.

## Reproducing the findings

Every claim above came from probes against the real validator. To re-derive them, enable the features:

```toml
jsonschema = { version = "0.17.1", default-features = false, features = ["draft201909", "draft202012"] }
```

then write a `#[tokio::test]` in `crates/taplo-common/src/schema/tests.rs` that seeds a schema through the existing `seeded()` helper and calls `schemas.validate(&url, &instance)`, printing `errors.len()`. Compare against the same test with the features removed. Traversal claims use `schemas.schemas_at_path(&url, &value, &keys)` with keys built as `"name".parse::<Keys>().unwrap().join(0_usize)`, since `Keys::from_str` parses TOML keys and will read a bare `0` as a key named "0" rather than an index.

Verified against `jsonschema` 0.17.1 at Taplo commit `93f6b4f`.
