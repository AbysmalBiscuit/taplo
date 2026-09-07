# JSON Schema Draft Version Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Taplo validate a schema against the draft it declares, warn instead of downgrading when it cannot, and resolve `prefixItems` during traversal so hover and completion work inside 2020-12 tuple arrays.

**Architecture:** Taplo reads `$schema` itself and passes an explicit `Draft` to `JSONSchema::options().with_draft`, which takes precedence over the dependency's own `$schema` sniffing and so routes around `draft_from_url` requiring a trailing `#`. The two `jsonschema` cargo features that compile the 2019-09 and 2020-12 `Draft` variants get enabled. Traversal, a separate code path from validation, gains a `prefixItems` branch.

**Tech Stack:** Rust 2021 (rust-version 1.74), `jsonschema` 0.17.1 (`default-features = false`), `serde_json`, `tokio` (test runtime), `tracing`.

**Spec:** docs/superpowers/specs/2026-09-07-json-schema-draft-versions.md

## Global Constraints

- `jsonschema` stays at exactly `0.17.1`. Upgrading is explicitly out of scope.
- Only two features may be added to it: `draft201909` and `draft202012`. Both are `[]` in the dependency's manifest, so `Cargo.lock` must not change.
- Only two crates are touched: `crates/taplo-common/Cargo.toml` and `crates/taplo-common/src/schema/`. No changes to `taplo-lsp`, `taplo-cli` or `taplo-wasm`.
- The unhonored-draft report is a `tracing::warn!` event, never a document diagnostic. Do not change the return type of `validate` or `validate_root`.
- Tests live in `crates/taplo-common/src/schema/tests.rs`, which is gated `#[cfg(all(test, feature = "reqwest"))]`. The dedicated command is `cargo test -p taplo-common --features schema,reqwest,rustls-tls`; `cargo test --workspace` also runs them, because `taplo-lsp` enables `schema` and `reqwest` on `taplo-common` and cargo unifies features.
- CI runs `cargo fmt --check`. Keep `schema/mod.rs` and `schema/tests.rs` rustfmt-clean. A pre-existing diff in `schema/associations.rs` is out of scope; leave it alone.
- Commit messages are Conventional Commits: imperative mood, lowercase after the colon, subject at most 50 characters.
- Comments state what the code does and the non-obvious why. Never reference this plan, a task number, an issue, or "previously"/"now we".
- Use absolute paths and `git -C /home/lev/Git/lev/taplo-wt/schema-draft-versions` for every command. Never `cd`. Never `git stash`. Never `git push`.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/taplo-common/src/schema/mod.rs` | `collect_schemas` array branch; `declared_draft` + `DeclaredDraft`; `create_validator` | 1, 2 |
| `crates/taplo-common/src/schema/tests.rs` | Traversal and validation tests, plus the `compiled_draft` helper | 1, 2 |
| `crates/taplo-common/Cargo.toml` | Enables the two `jsonschema` draft features | 2 |
| `docs/superpowers/specs/2026-09-07-json-schema-draft-versions.md` | Records the measured dependency cost | 3 |

The worktree is at `/home/lev/Git/lev/taplo-wt/schema-draft-versions`; every path above is relative to it.

## Baseline

Green at the branch point, re-derived before any change:

- `cargo check --workspace --all-targets` — exit 0
- `cargo test --workspace` — exit 0
- `cargo test -p taplo-common --features schema,reqwest,rustls-tls` — 3 passed
- stripped release `taplo`: 11,834,800 bytes

---

## Task 1: Resolve `prefixItems` during traversal

`collect_schemas` reads `items` for an array position and never `prefixItems`, so hover and completion return nothing inside a 2020-12 tuple array. Under 2020-12 the two keywords partition the array: `prefixItems[idx]` applies while `idx < prefixItems.len()`, and single-schema `items` applies only past that. Traversal follows the same partition.

**Files:**
- Modify: `crates/taplo-common/src/schema/mod.rs` (the `KeyOrIndex::Index` arm of `collect_schemas`, around lines 437-460)
- Test: `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Consumes: `seeded(schema) -> (Schemas<NativeEnvironment>, Url)` and `TEST_SCHEMA_URL`, both already in `tests.rs`.
- Produces: `fn descriptions(schemas: &[(Keys, Arc<Value>)]) -> Vec<&str>` in `tests.rs`, reused by no later task. No change to any public signature.

Note for the implementer: `Keys::from_str` parses TOML keys, so a bare `0` would parse as a key *named* `"0"`, not an index. Build an index with `.join(0_usize)`, as the tests below do.

- [ ] **Step 1: Write the failing tests**

Append to `crates/taplo-common/src/schema/tests.rs`:

```rust
/// Collects `description` from each schema so assertions name which branch
/// of the schema was reached rather than comparing whole JSON values.
fn descriptions(schemas: &[(Keys, Arc<Value>)]) -> Vec<&str> {
    schemas
        .iter()
        .filter_map(|(_, s)| s["description"].as_str())
        .collect()
}

#[tokio::test]
async fn prefix_items_resolves_at_covered_index() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "pair": {
                "type": "array",
                "prefixItems": [
                    { "type": "string", "description": "head" },
                    { "type": "integer", "description": "second" }
                ]
            }
        }
    }))
    .await;

    let keys = "pair".parse::<Keys>().unwrap().join(0_usize);
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["head"]);
}

#[tokio::test]
async fn prefix_items_falls_through_to_items_past_the_end() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "pair": {
                "type": "array",
                "prefixItems": [{ "type": "string", "description": "head" }],
                "items": { "type": "integer", "description": "tail" }
            }
        }
    }))
    .await;

    let keys = "pair".parse::<Keys>().unwrap().join(1_usize);
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["tail"]);
}

#[tokio::test]
async fn items_does_not_apply_at_an_index_prefix_items_covers() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "pair": {
                "type": "array",
                "prefixItems": [{ "type": "string", "description": "head" }],
                "items": { "type": "integer", "description": "tail" }
            }
        }
    }))
    .await;

    let keys = "pair".parse::<Keys>().unwrap().join(0_usize);
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["head"]);
}

#[tokio::test]
async fn draft_7_tuple_items_still_resolve_by_position() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "pair": {
                "type": "array",
                "items": [
                    { "type": "string", "description": "head" },
                    { "type": "integer", "description": "second" }
                ]
            }
        }
    }))
    .await;

    let keys = "pair".parse::<Keys>().unwrap().join(1_usize);
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["second"]);
}

#[tokio::test]
async fn single_schema_items_still_resolve_at_every_index() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "list": {
                "type": "array",
                "items": { "type": "string", "description": "every element" }
            }
        }
    }))
    .await;

    let keys = "list".parse::<Keys>().unwrap().join(3_usize);
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["every element"]);
}
```

- [ ] **Step 2: Run the tests and confirm they fail for the right reason**

```bash
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml \
  -p taplo-common --features schema,reqwest,rustls-tls prefix_items
```

Expected: `prefix_items_resolves_at_covered_index` fails with `assertion \`left == right\` failed: left: [], right: ["head"]` — traversal found no schema at all, because `prefixItems` is not read. `items_does_not_apply_at_an_index_prefix_items_covers` fails with `left: ["tail"]` — the wrong branch was reached. `prefix_items_falls_through_to_items_past_the_end` already passes, since `items` alone covers index 1 today; that is expected and it guards against a regression in step 3.

The two draft-7 tests must pass before the change. Confirm:

```bash
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml \
  -p taplo-common --features schema,reqwest,rustls-tls items_still_resolve
```

Expected: 2 passed.

If a failure message differs from the above — a compile error, a panic in `Keys` parsing — stop and fix the test before touching `mod.rs`. A test that fails for the wrong reason proves nothing.

- [ ] **Step 3: Teach the index branch about `prefixItems`**

In `crates/taplo-common/src/schema/mod.rs`, replace the whole `KeyOrIndex::Index(idx)` arm of `collect_schemas`:

```rust
            KeyOrIndex::Index(idx) => {
                if schema["items"].is_array() {
                    self.collect_schemas(
                        root_url,
                        &schema["items"][idx],
                        &value[idx],
                        full_path.join(*idx),
                        &child_path,
                        schemas,
                    )
                    .await?;
                } else {
                    self.collect_schemas(
                        root_url,
                        &schema["items"],
                        &value[idx],
                        full_path.join(*idx),
                        &child_path,
                        schemas,
                    )
                    .await?;
                }
            }
```

with:

```rust
            KeyOrIndex::Index(idx) => {
                // `prefixItems` and `items` partition the array: the leading
                // positions belong to `prefixItems`, the rest to `items`.
                let covered_by_prefix_items = schema["prefixItems"]
                    .as_array()
                    .is_some_and(|prefix_items| *idx < prefix_items.len());

                let item_schema = if covered_by_prefix_items {
                    &schema["prefixItems"][idx]
                } else if schema["items"].is_array() {
                    &schema["items"][idx]
                } else {
                    &schema["items"]
                };

                self.collect_schemas(
                    root_url,
                    item_schema,
                    &value[idx],
                    full_path.join(*idx),
                    &child_path,
                    schemas,
                )
                .await?;
            }
```

Do not add a `prefixItems` counterpart to the `KeyOrIndex::Key` arm. That arm reads `schema["items"][k]` for arrays of tables, and indexing an array by a key name is always `Null`.

- [ ] **Step 4: Run the tests and confirm they pass**

```bash
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml \
  -p taplo-common --features schema,reqwest,rustls-tls
```

Expected: 8 passed (the 3 pre-existing plus the 5 new).

- [ ] **Step 5: Confirm nothing else broke**

```bash
cargo check --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml --workspace --all-targets
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml --workspace
cargo fmt --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml -p taplo-common --check
```

Expected: the first two exit 0 with no new warnings in `taplo-common`. `cargo fmt --check` must report no diff in `schema/mod.rs` or `schema/tests.rs`; a diff in `schema/associations.rs` is pre-existing and out of scope.

- [ ] **Step 6: Commit**

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-draft-versions add \
  crates/taplo-common/src/schema/mod.rs \
  crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-draft-versions commit -m "feat(schema): resolve prefixItems in traversal" -m "Traversal read only \`items\` for an array position, so hover and
completion returned nothing inside a 2020-12 tuple array even where
validation understood the schema.

Partition the array the way 2020-12 does: \`prefixItems\` covers the
leading positions, \`items\` covers the rest. Collecting both at a
covered index would offer the tail schema in a head position."
```

---

## Task 2: Validate against the declared draft

Two defects combine into one silent downgrade. The `draft201909` and `draft202012` cargo features are off, so those `Draft` variants do not exist; and `jsonschema::schemas::draft_from_url` matches `$schema` as an exact string requiring a trailing `#`, which the canonical 2020-12 value does not carry. Taplo detects the draft itself and passes it explicitly, which wins over the dependency's own sniffing.

Enabling the features alone changes behavior, because a `#`-suffixed 2020-12 `$schema` starts compiling as 2020-12, which switches `format` validation off. The feature flip, the explicit draft, and the `should_validate_formats(true)` guard therefore land together.

**Files:**
- Modify: `crates/taplo-common/Cargo.toml` (the `jsonschema` dependency line)
- Modify: `crates/taplo-common/src/schema/mod.rs` (the `jsonschema` import, `create_validator`, and a new `declared_draft` free function)
- Test: `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Consumes: `seeded()` and `descriptions()` from Task 1's `tests.rs`.
- Produces, in `mod.rs`:
  - `enum DeclaredDraft { Supported(Draft), Unsupported(String), Unrecognized }` — private to the module, derives `Debug, PartialEq, Eq`.
  - `fn declared_draft(schema: &Value) -> DeclaredDraft` — private free function.
  - `create_validator` keeps its signature: `fn create_validator(&self, schema: &Value) -> Result<JSONSchema, anyhow::Error>`.
- Produces, in `tests.rs`: `fn compiled_draft(schemas: &Schemas<NativeEnvironment>, url: &Url) -> Draft`.

Note for the implementer: `tests.rs` is a child module of `schema`, so it can read the private `Schemas::validators` field and call the private `declared_draft`. `JSONSchema::draft()` is public and returns the `Draft` the schema was compiled against, which is how the tests observe the draft directly rather than inferring it from error counts.

- [ ] **Step 1: Enable the two features**

In `crates/taplo-common/Cargo.toml`, replace:

```toml
jsonschema         = { version = "0.17.1", default-features = false }
```

with:

```toml
jsonschema         = { version = "0.17.1", default-features = false, features = ["draft201909", "draft202012"] }
```

Confirm `Cargo.lock` is untouched, since both features are empty:

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-draft-versions status --short
```

Expected: `Cargo.lock` does not appear.

- [ ] **Step 2: Write the failing tests**

Append to `crates/taplo-common/src/schema/tests.rs`:

`Draft` needs no `use` line here: `tests.rs` already opens with `use super::*;`, and step 4 adds `Draft` to the `jsonschema` import in `mod.rs`. Adding a second explicit import would shadow the glob for no reason.

```rust
/// Reads the draft the cached validator was compiled against, which is the
/// only direct evidence of which draft's rules a schema ran under.
fn compiled_draft(schemas: &Schemas<NativeEnvironment>, url: &Url) -> Draft {
    schemas
        .validators
        .lock()
        .get(url)
        .cloned()
        .expect("no validator was cached")
        .draft()
}

/// `{"a": "x", "b": 1}` leaves `b` unevaluated, which only 2019-09 and
/// 2020-12 report.
fn unevaluated_properties_schema(declared: &str) -> Value {
    json!({
        "$schema": declared,
        "type": "object",
        "properties": { "a": { "type": "string" } },
        "unevaluatedProperties": false
    })
}

async fn errors_for(schema: Value) -> (Vec<String>, Draft) {
    let (schemas, url) = seeded(schema).await;
    let instance = json!({ "a": "x", "b": 1 });
    let errors = schemas.validate(&url, &instance).await.unwrap();
    let draft = compiled_draft(&schemas, &url);
    (errors.iter().map(ToString::to_string).collect(), draft)
}

#[tokio::test]
async fn draft_2020_12_without_a_fragment_is_honored() {
    let (errors, draft) = errors_for(unevaluated_properties_schema(
        "https://json-schema.org/draft/2020-12/schema",
    ))
    .await;

    assert_eq!(draft, Draft::Draft202012);
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[tokio::test]
async fn draft_2020_12_with_a_fragment_is_honored() {
    let (errors, draft) = errors_for(unevaluated_properties_schema(
        "https://json-schema.org/draft/2020-12/schema#",
    ))
    .await;

    assert_eq!(draft, Draft::Draft202012);
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[tokio::test]
async fn draft_2019_09_is_honored() {
    let (errors, draft) = errors_for(unevaluated_properties_schema(
        "https://json-schema.org/draft/2019-09/schema",
    ))
    .await;

    assert_eq!(draft, Draft::Draft201909);
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[tokio::test]
async fn an_unsupported_draft_falls_back_to_draft_7() {
    let (_, draft) = errors_for(unevaluated_properties_schema(
        "http://json-schema.org/draft-03/schema#",
    ))
    .await;

    assert_eq!(draft, Draft::Draft7);
}

#[tokio::test]
async fn a_schema_without_a_declaration_is_draft_7() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": { "a": { "type": "string" } }
    }))
    .await;

    schemas.validate(&url, &json!({ "a": "x" })).await.unwrap();

    assert_eq!(compiled_draft(&schemas, &url), Draft::Draft7);
}

#[tokio::test]
async fn custom_formats_still_assert_under_draft_2020_12() {
    let (schemas, url) = seeded(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": { "version": { "type": "string", "format": "semver" } }
    }))
    .await;

    let errors = schemas
        .validate(&url, &json!({ "version": "not a version" }))
        .await
        .unwrap();

    assert_eq!(compiled_draft(&schemas, &url), Draft::Draft202012);
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn declared_draft_classifies_every_meta_schema_uri() {
    let classify = |declared: &str| declared_draft(&json!({ "$schema": declared }));

    assert_eq!(
        classify("http://json-schema.org/draft-07/schema#"),
        DeclaredDraft::Supported(Draft::Draft7)
    );
    assert_eq!(
        classify("https://json-schema.org/draft-07/schema"),
        DeclaredDraft::Supported(Draft::Draft7)
    );
    assert_eq!(
        classify("http://json-schema.org/draft-04/schema#"),
        DeclaredDraft::Supported(Draft::Draft4)
    );
    assert_eq!(
        classify("http://json-schema.org/draft-06/schema#"),
        DeclaredDraft::Supported(Draft::Draft6)
    );
    assert_eq!(
        classify("https://json-schema.org/draft/2019-09/schema"),
        DeclaredDraft::Supported(Draft::Draft201909)
    );
    assert_eq!(
        classify("https://json-schema.org/draft/2020-12/schema"),
        DeclaredDraft::Supported(Draft::Draft202012)
    );

    assert_eq!(
        classify("http://json-schema.org/draft-03/schema#"),
        DeclaredDraft::Unsupported("http://json-schema.org/draft-03/schema".to_owned())
    );
    assert_eq!(
        classify("https://json-schema.org/draft/next/schema"),
        DeclaredDraft::Unsupported("https://json-schema.org/draft/next/schema".to_owned())
    );

    assert_eq!(
        classify("https://example.com/my-meta-schema.json"),
        DeclaredDraft::Unrecognized
    );
    assert_eq!(classify("not a url"), DeclaredDraft::Unrecognized);
    assert_eq!(declared_draft(&json!({})), DeclaredDraft::Unrecognized);
    assert_eq!(declared_draft(&Value::Bool(true)), DeclaredDraft::Unrecognized);
}
```

- [ ] **Step 3: Run the tests and confirm they fail for the right reason**

```bash
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml \
  -p taplo-common --features schema,reqwest,rustls-tls 2>&1 | tail -40
```

Expected: a compile error — `cannot find function \`declared_draft\` in this scope`, `cannot find type \`DeclaredDraft\` in this scope`, and `cannot find type \`Draft\` in this scope`, the last because step 4 has not yet added `Draft` to the `jsonschema` import in `mod.rs`. That is the right failure: nothing under test exists yet.

To see the runtime failure the feature actually fixes, temporarily add `use jsonschema::Draft;` to `tests.rs`, comment out `declared_draft_classifies_every_meta_schema_uri`, and re-run. Expected then: `draft_2020_12_without_a_fragment_is_honored` and `draft_2019_09_is_honored` fail with `left: Draft7, right: Draft202012` / `Draft201909`, because `draft_from_url` demands the trailing `#`. `draft_2020_12_with_a_fragment_is_honored` passes already — enabling the features in step 1 is enough for that one — and `custom_formats_still_assert_under_draft_2020_12` fails on its first assertion with `left: Draft7, right: Draft202012`, for the same trailing-`#` reason. The format regression it guards is not visible yet: it appears only once the draft is honored, and step 4's `should_validate_formats(true)` is what then keeps this test at 1 error instead of 0. To see that for yourself, finish step 4, delete the `should_validate_formats(true)` line, and re-run: the test fails `left: 0, right: 1`. Put the line back.

Remove the temporary import and uncomment the test before continuing.

- [ ] **Step 4: Detect the draft and pass it explicitly**

In `crates/taplo-common/src/schema/mod.rs`, change the `jsonschema` import from:

```rust
use jsonschema::{error::ValidationErrorKind, JSONSchema, SchemaResolver, ValidationError};
```

to:

```rust
use jsonschema::{error::ValidationErrorKind, Draft, JSONSchema, SchemaResolver, ValidationError};
```

Replace `create_validator`:

```rust
    fn create_validator(&self, schema: &Value) -> Result<JSONSchema, anyhow::Error> {
        JSONSchema::options()
            .with_resolver(CacheSchemaResolver {
                cache: self.cache().clone(),
            })
            .with_format("semver", formats::semver)
            .with_format("semver-requirement", formats::semver_req)
            .compile(schema)
            .map_err(|err| anyhow!("invalid schema: {err}"))
    }
```

with:

```rust
    fn create_validator(&self, schema: &Value) -> Result<JSONSchema, anyhow::Error> {
        let mut options = JSONSchema::options();

        options
            .with_resolver(CacheSchemaResolver {
                cache: self.cache().clone(),
            })
            .with_format("semver", formats::semver)
            .with_format("semver-requirement", formats::semver_req)
            // `format` is an annotation from 2019-09 on, so setting one of
            // those drafts would otherwise stop every format from asserting,
            // including the two registered above.
            .should_validate_formats(true);

        match declared_draft(schema) {
            // An explicit draft takes precedence over the `$schema` sniffing
            // in `jsonschema`, which matches meta-schema URIs exactly and so
            // misses the fragment-less form that 2019-09 and 2020-12 use.
            DeclaredDraft::Supported(draft) => {
                options.with_draft(draft);
            }
            DeclaredDraft::Unsupported(declared) => {
                tracing::warn!(
                    %declared,
                    used = "draft-07",
                    "schema declares a draft taplo cannot validate, validating as draft-07 instead"
                );
            }
            DeclaredDraft::Unrecognized => {}
        }

        options
            .compile(schema)
            .map_err(|err| anyhow!("invalid schema: {err}"))
    }
```

Add, immediately after the `reference_url` function near the bottom of the file:

```rust
/// How a schema's `$schema` value maps onto a draft taplo can validate against.
#[derive(Debug, PartialEq, Eq)]
enum DeclaredDraft {
    Supported(Draft),
    /// A meta-schema taplo recognizes as one but has no validator for. Carries
    /// the declared URI so the warning can name it.
    Unsupported(String),
    Unrecognized,
}

/// Classifies the root `$schema`, which is the only one that counts: a
/// compiled `JSONSchema` carries a single draft, and every document reached
/// through `$ref` is compiled under it.
fn declared_draft(schema: &Value) -> DeclaredDraft {
    let Some(declared) = schema["$schema"].as_str() else {
        return DeclaredDraft::Unrecognized;
    };

    // Classification goes by host and path, so scheme, case, query and
    // fragment do not matter. Stripping the trailing `#` that the canonical
    // form carries up to draft-07 only keeps the reported URI tidy.
    let normalized = declared.strip_suffix('#').unwrap_or(declared);

    let Ok(url) = Url::parse(normalized) else {
        return DeclaredDraft::Unrecognized;
    };

    if url.host_str() != Some("json-schema.org") {
        return DeclaredDraft::Unrecognized;
    }

    match url.path() {
        "/draft-04/schema" => DeclaredDraft::Supported(Draft::Draft4),
        "/draft-06/schema" => DeclaredDraft::Supported(Draft::Draft6),
        "/draft-07/schema" => DeclaredDraft::Supported(Draft::Draft7),
        "/draft/2019-09/schema" => DeclaredDraft::Supported(Draft::Draft201909),
        "/draft/2020-12/schema" => DeclaredDraft::Supported(Draft::Draft202012),
        _ => DeclaredDraft::Unsupported(normalized.to_owned()),
    }
}
```

- [ ] **Step 5: Run the tests and confirm they pass**

```bash
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml \
  -p taplo-common --features schema,reqwest,rustls-tls
```

Expected: 15 passed (3 pre-existing, 5 from Task 1, 7 new).

If `declared_draft` or `DeclaredDraft` is reported as dead code, the tests are not reaching them — check that `tests.rs` uses the bare names, which resolve through the `use super::*;` already at the top of that file.

- [ ] **Step 6: Confirm nothing else broke**

```bash
cargo check --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml --workspace --all-targets
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml --workspace
cargo fmt --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml -p taplo-common --check
git -C /home/lev/Git/lev/taplo-wt/schema-draft-versions status --short
```

Expected: the two build commands exit 0; `cargo fmt --check` reports no diff in `schema/mod.rs` or `schema/tests.rs` (a diff in `schema/associations.rs` is pre-existing); `Cargo.lock` is absent from `git status`.

- [ ] **Step 7: Commit**

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-draft-versions add \
  crates/taplo-common/Cargo.toml \
  crates/taplo-common/src/schema/mod.rs \
  crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-draft-versions commit -m "feat(schema): honor the declared schema draft" -m "Every schema validated as draft 7 whatever it declared. The 2019-09
and 2020-12 cargo features were off, so those variants did not exist,
and \`draft_from_url\` matches meta-schema URIs exactly and demands a
trailing \`#\` that the canonical 2020-12 value omits.

Enable both features and classify \`\$schema\` in taplo, passing the
draft explicitly so it wins over that sniffing. A meta-schema on
json-schema.org with no validator behind it now warns instead of
downgrading in silence.

Force format validation on, since it is an annotation from 2019-09 on
and would otherwise stop asserting, taking the registered semver
formats with it."
```

---

## Task 3: Record the dependency cost and reconcile the spec

The tracking issue asks to assess the dependency cost of the two features before committing to them. The features are `[]` in the `jsonschema` manifest, so they add no crates and leave `Cargo.lock` alone; what they add is compiled code. The spec has a table with the baseline filled in and the post-change figure left open. Three sentences in the spec also describe the implementation slightly wrong and are corrected here, against the code as built.

**Files:**
- Modify: `docs/superpowers/specs/2026-09-07-json-schema-draft-versions.md` (the "Dependency cost" table and three prose corrections)

**Interfaces:** none. Documentation only.

- [ ] **Step 1: Measure the release binary with the features on**

```bash
cargo build --release --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml -p taplo-cli
ls -l /home/lev/Git/lev/taplo-wt/schema-draft-versions/target/release/taplo | awk '{print $5}'
```

Expected: a byte count somewhat above the 11,834,800-byte baseline. Record the exact number.

- [ ] **Step 2: Fill in the table**

In `docs/superpowers/specs/2026-09-07-json-schema-draft-versions.md`, replace:

```markdown
| with both features | *(recorded during implementation)* |
```

with the measured figure, formatted like the baseline row, and add a sentence after the table giving the delta in bytes and as a percentage. Do not invent a number; use what step 1 printed.

- [ ] **Step 3: Correct three sentences that no longer match the code**

In the same spec file, replace:

```markdown
Normalization strips one trailing `#`, and nothing else. A `$schema` value that differs from a known meta-schema URI in any other way stays unrecognized, which is the correct outcome.
```

with:

```markdown
Classification compares the host and path of the parsed URI, so scheme, case, query and fragment do not matter. A different path on `json-schema.org` is unsupported; a different host is unrecognized.
```

Replace:

```markdown
    Unsupported(&'static str),
```

with:

```markdown
    Unsupported(String),
```

The payload is the `$schema` value read at runtime, so it cannot be `&'static str`.

Replace:

```markdown
A schema that declares draft-04 but uses draft-6 constructs compiles today and will be rejected as an invalid schema.
```

with:

```markdown
A schema that declares draft-04 but uses later constructs either has them ignored (`const`, `contains`, `propertyNames`, `if`, which the draft-4 meta-schema does not know) or is rejected as an invalid schema (numeric `exclusiveMaximum`, boolean subschemas, which the draft-4 meta-schema forbids).
```

- [ ] **Step 4: Commit**

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-draft-versions add docs/superpowers/specs/2026-09-07-json-schema-draft-versions.md
git -C /home/lev/Git/lev/taplo-wt/schema-draft-versions commit -m "docs: record the draft feature binary cost"
```

---

## Verification

After all three tasks:

```bash
cargo check --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml --workspace --all-targets
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml --workspace
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-draft-versions/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls
git -C /home/lev/Git/lev/taplo-wt/schema-draft-versions status --short
```

Expected: the first two exit 0, the third reports 15 passed, and `git status` is empty.

Acceptance criteria to tasks:

| Criterion | Task | Evidence |
|---|---|---|
| 1, 2 (2020-12 and 2019-09 honored) | 2 | `draft_2020_12_*`, `draft_2019_09_is_honored` |
| 3 (unsupported draft warns, runs as draft 7) | 2 | `an_unsupported_draft_falls_back_to_draft_7`, `declared_draft_classifies_every_meta_schema_uri` |
| 4 (no `$schema` is draft 7) | 2 | `a_schema_without_a_declaration_is_draft_7` |
| 5, 6 (`prefixItems` traversal, `items` unchanged) | 1 | the five traversal tests |
| 7 (workspace builds and tests) | 1, 2 | the commands above |
| 8 (custom formats still assert) | 2 | `custom_formats_still_assert_under_draft_2020_12` |
| 9 (dependency cost measured) | 3 | the table in the spec |
