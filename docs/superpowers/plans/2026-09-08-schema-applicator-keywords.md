# JSON Schema applicator keywords Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make schema traversal read `if`/`then`/`else`, `dependencies`, `dependentSchemas` and `unevaluatedProperties`, and make hover state `not`, `propertyNames` and `contains` without traversal ever mistaking them for requirements.

**Architecture:** `collect_schemas` already carries the instance at every position; the in-place applicators read it to pick their branch, falling back to every branch when the instance is absent or the condition cannot be decided. `collect_child_schemas` gains the same instance so completion and hover never disagree at the position they share. `not`, `propertyNames` and `contains` are never descended into; hover renders each as a labelled block of the subschema's own facts.

**Tech Stack:** Rust, `serde_json`, `jsonschema` 0.17.1, `async_recursion`, `lsp-types`, `tokio` tests.

**Spec:** `docs/superpowers/specs/2026-09-08-schema-applicator-keywords.md`

## Global Constraints

- Work only in the worktree `/home/lev/Git/lev/taplo-wt/schema-applicators`, on branch `feat/schema-applicators`. Use `git -C /home/lev/Git/lev/taplo-wt/schema-applicators …` and absolute paths in every command. Never `cd` first. Never `git stash`; read another revision with `git show REV:path`.
- Commit, do not push. No `git push`, no pull request, no issue edits.
- Conventional Commits, imperative mood, lowercase after the colon, subject ≤ 50 characters. A body only where the change needs context. End every commit message with `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.
- Comments are timeless: what the code does and the non-obvious why. Never "this PR", "now we", "previously", never a task or issue number.
- `rg` not `grep`, `fd` not `find`. `rg -r` means `--replace`, so line numbers are `rg -n`.
- `cargo fmt --check` is not clean at the branch point. Format only the files you touch: `cargo fmt -p taplo-common` / `-p taplo-lsp` is too broad — run `rustfmt --edition 2021 <file>` on the exact files changed, or check the diff by hand.
- Two `dead_code` warnings in `taplo` and `lsp-async-stub` are inherited and expected.
- `taplo-common`'s schema tests only build with features: `cargo test -p taplo-common --features schema,reqwest,rustls-tls`.
- The spec's commit 2 lands here as two commits (Tasks 2 and 3), because the traversal change and the handler change are independently rejectable.

---

### Task 1: Bound composition depth in `collect_schemas`

`collect_schemas` recurses on `$ref`, `allOf`, `oneOf` and `anyOf` without consuming any path, so a cycle never terminates. `3cadcb4` gave `collect_child_schemas` a budget for exactly this and left `collect_schemas` without one. Every applicator added later in this plan is another in-place recursion, so the budget lands first.

**Files:**
- Modify: `crates/taplo-common/src/schema/mod.rs` (`schemas_at_path`, `collect_schemas`)
- Test: `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Consumes: `MAX_COMPOSITION_DEPTH` (already defined at the top of `mod.rs`).
- Produces: `collect_schemas(&self, root_url, schema, value, full_path, path, composition_depth, schemas)` — one new `usize` parameter before `schemas`.

- [ ] **Step 1: Write the failing test**

Append to `crates/taplo-common/src/schema/tests.rs`:

```rust
#[tokio::test]
async fn self_referential_all_of_terminates_at_a_non_empty_path() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "node": { "$ref": "#/definitions/node" }
        },
        "definitions": {
            "node": {
                "description": "Points back at itself.",
                "allOf": [{ "$ref": "#/definitions/node" }]
            }
        }
    }))
    .await;

    let keys = "node".parse::<Keys>().unwrap();

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["Points back at itself."]);
}
```

- [ ] **Step 2: Run the test and watch it fail**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-common --features schema,reqwest,rustls-tls self_referential_all_of_terminates_at_a_non_empty_path`

Expected: the process aborts with `has overflowed its stack` / `fatal runtime error: stack overflow`. That is the right failure. A plain assertion failure means the schema was written wrong.

- [ ] **Step 3: Add the budget**

In `crates/taplo-common/src/schema/mod.rs`, change the `collect_schemas` call inside `schemas_at_path` from

```rust
        self.collect_schemas(
            schema_url,
            &schema,
            value,
            Keys::empty(),
            path,
            &mut schemas,
        )
        .await?;
```

to

```rust
        self.collect_schemas(
            schema_url,
            &schema,
            value,
            Keys::empty(),
            path,
            MAX_COMPOSITION_DEPTH,
            &mut schemas,
        )
        .await?;
```

Change the signature and the guard:

```rust
    #[tracing::instrument(skip_all, fields(%path))]
    #[async_recursion(?Send)]
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    async fn collect_schemas(
        &self,
        root_url: &Url,
        schema: &Value,
        value: &Value,
        full_path: Keys,
        path: &Keys,
        composition_depth: usize,
        schemas: &mut Vec<(Keys, Arc<Value>)>,
    ) -> Result<(), anyhow::Error> {
        if !schema.is_object() || composition_depth == 0 {
            return Ok(());
        }

        let composition_depth = composition_depth - 1;
```

Then pass `composition_depth` to the four recursions that keep the path — the `$ref` tail call and the `oneOf`, `anyOf` and `allOf` loops — and `MAX_COMPOSITION_DEPTH` to the five that consume a path segment: the `items[k]`, `properties[k]`, `additionalProperties` and `patternProperties` descents in the `KeyOrIndex::Key` arm, and the item descent in the `KeyOrIndex::Index` arm.

Extend the doc comment on `MAX_COMPOSITION_DEPTH` so it no longer reads as though only one traversal uses it:

```rust
/// `$ref`, `allOf`, `oneOf`, `anyOf` and the conditional applicators can point
/// back at the schema that contains them. Such a cycle makes no progress
/// against the traversal depth, which only counts property nesting, so
/// composition gets its own budget. Both traversals spend it, and both reset
/// it wherever a descent consumes a path segment.
const MAX_COMPOSITION_DEPTH: usize = 32;
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-common --features schema,reqwest,rustls-tls`

Expected: 16 passed, 0 failed.

- [ ] **Step 5: Check the workspace still builds**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo check --workspace --all-targets && cargo test --workspace 2>&1 | rg "^test result: FAILED" ; echo "exit $?"`

Expected: `cargo check` clean; the `rg` finds nothing (exit 1 from `rg`, which is the pass condition here).

- [ ] **Step 6: Commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-common/src/schema/mod.rs /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators add crates/taplo-common/src/schema/mod.rs crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators commit -m "fix(schema): bound composition depth in collect_schemas" -m "A schema whose allOf points back at itself made collect_schemas recurse
forever, so hovering a key with that shape aborted the language server
with a stack overflow. collect_child_schemas has carried a budget for
the same cycle since it was found there; collect_schemas never got one.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 2: Pick the applicable `if` / `then` / `else` branch

**Files:**
- Modify: `crates/taplo-common/src/schema/mod.rs` (`collect_schemas`, `collect_child_schemas`, `possible_schemas_from`, new free functions and two new methods)
- Test: `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Consumes: `collect_schemas`'s `composition_depth` parameter from Task 1; the existing `ref_schema_value`, `create_validator`, `resolve_schema`.
- Produces:
  - `fn instance_at<'v>(value: &'v Value, keys: &Keys) -> &'v Value`
  - `fn names_a_ref(schema: &Value) -> bool`
  - `async fn Schemas::condition_holds(&self, root_url: &Url, condition: &Value, instance: &Value) -> Option<bool>`
  - `async fn Schemas::conditional_subschemas<'s>(&self, root_url: &Url, schema: &'s Value, instance: &Value) -> Vec<&'s Value>`
  - `collect_child_schemas(&self, root_url, schema, root_path, path, instance, depth, composition_depth, schemas)` — one new `&Value` parameter after `path`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/taplo-common/src/schema/tests.rs`:

```rust
/// A schema whose `image` key is described differently by each branch of one
/// condition, so that the description names the branch that was taken.
fn conditional_schema() -> Value {
    json!({
        "type": "object",
        "if": {
            "properties": { "kind": { "const": "docker" } },
            "required": ["kind"]
        },
        "then": { "properties": { "image": { "type": "string", "description": "then" } } },
        "else": { "properties": { "image": { "type": "string", "description": "else" } } }
    })
}

#[tokio::test]
async fn a_met_condition_selects_the_then_branch() {
    let (schemas, url) = seeded(conditional_schema()).await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "docker" }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["then"]);
}

#[tokio::test]
async fn an_unmet_condition_selects_the_else_branch() {
    let (schemas, url) = seeded(conditional_schema()).await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "podman" }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["else"]);
}

#[tokio::test]
async fn an_absent_instance_takes_both_branches() {
    let (schemas, url) = seeded(conditional_schema()).await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["then", "else"]);
}

#[tokio::test]
async fn an_empty_instance_decides_the_condition() {
    let (schemas, url) = seeded(conditional_schema()).await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({}), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["else"]);
}

#[tokio::test]
async fn a_condition_that_is_a_reference_is_resolved() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": { "$ref": "#/definitions/docker" },
        "then": { "properties": { "image": { "description": "then" } } },
        "else": { "properties": { "image": { "description": "else" } } },
        "definitions": {
            "docker": {
                "properties": { "kind": { "const": "docker" } },
                "required": ["kind"]
            }
        }
    }))
    .await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "docker" }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["then"]);
}

#[tokio::test]
async fn a_condition_holding_a_nested_reference_takes_both_branches() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": {
            "properties": { "kind": { "$ref": "#/definitions/docker" } },
            "required": ["kind"]
        },
        "then": { "properties": { "image": { "description": "then" } } },
        "else": { "properties": { "image": { "description": "else" } } },
        "definitions": { "docker": { "const": "docker" } }
    }))
    .await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "docker" }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["then", "else"]);
}

#[tokio::test]
async fn a_condition_that_does_not_compile_takes_both_branches() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": { "pattern": "(" },
        "then": { "properties": { "image": { "description": "then" } } },
        "else": { "properties": { "image": { "description": "else" } } }
    }))
    .await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "docker" }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["then", "else"]);
}

#[tokio::test]
async fn a_draft_4_root_still_decides_a_const_condition() {
    let mut schema = conditional_schema();
    schema["$schema"] = json!("http://json-schema.org/draft-04/schema#");
    let (schemas, url) = seeded(schema).await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "podman" }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["else"]);
}

#[tokio::test]
async fn branches_without_a_condition_are_inert() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "then": { "properties": { "image": { "description": "then" } } },
        "else": { "properties": { "image": { "description": "else" } } }
    }))
    .await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({}), &keys)
        .await
        .unwrap();

    assert!(found.is_empty());
}

#[tokio::test]
async fn the_schema_carrying_a_condition_is_still_collected() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "server": {
                "description": "carrier",
                "if": { "properties": { "kind": { "const": "docker" } }, "required": ["kind"] },
                "then": { "description": "then" }
            }
        }
    }))
    .await;

    let keys = "server".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "server": { "kind": "docker" } }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["then", "carrier"]);
}

#[tokio::test]
async fn completion_follows_the_branch_the_document_selects() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": { "properties": { "kind": { "const": "docker" } }, "required": ["kind"] },
        "then": { "properties": { "image": { "type": "string" } } },
        "else": { "properties": { "binary": { "type": "string" } } }
    }))
    .await;

    let children = schemas
        .possible_schemas_from(&url, &json!({ "kind": "docker" }), &Keys::empty(), 5)
        .await
        .unwrap();

    let mut offered: Vec<String> = children
        .iter()
        .map(|(_, relative, _)| relative.to_string())
        .filter(|key| !key.is_empty())
        .collect();
    offered.sort();

    assert_eq!(offered, ["image"]);
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-common --features schema,reqwest,rustls-tls`

Expected: `branches_without_a_condition_are_inert` passes (nothing descends into `then` yet). Every other new test fails with an empty or wrong `descriptions` vector — `assertion \`left == right\` failed: left: [], right: ["then"]` and so on. A compile error means the test code was mistyped.

- [ ] **Step 3: Add the free functions**

In `crates/taplo-common/src/schema/mod.rs`, beside `reference_url`:

```rust
/// The part of the document a schema at `keys` applies to.
///
/// Indexing a `Value` with a missing key or an out-of-range index yields
/// `Value::Null`, which is how an absent position reports itself.
fn instance_at<'v>(value: &'v Value, keys: &Keys) -> &'v Value {
    let mut instance = value;

    for key in keys.iter() {
        instance = match key {
            KeyOrIndex::Key(k) => &instance[k.value()],
            KeyOrIndex::Index(idx) => &instance[*idx],
        };
    }

    instance
}

/// Whether a subschema names a reference anywhere within it.
///
/// A subschema compiled on its own keeps its `#/...` pointers and loses the
/// document they point into, so every reference fails to resolve and the
/// subschema rejects every instance. A condition that names one is therefore
/// undecidable rather than false.
fn names_a_ref(schema: &Value) -> bool {
    match schema {
        Value::Object(map) => {
            map.get("$ref").is_some_and(Value::is_string) || map.values().any(names_a_ref)
        }
        Value::Array(items) => items.iter().any(names_a_ref),
        _ => false,
    }
}
```

- [ ] **Step 4: Add the two methods**

In the same `impl<E: Environment> Schemas<E>` block that holds `collect_schemas`, above it:

```rust
    /// Whether the instance satisfies a condition, or `None` when the
    /// condition cannot be decided and both branches have to be offered.
    ///
    /// A condition that is itself a reference is resolved, one hop, the way
    /// traversal resolves any schema carrying `$ref`. The condition compiles
    /// through `create_validator`, which sees no `$schema` on a subschema and
    /// so compiles it as draft 7 whatever the root declares — the floor a
    /// draft-4 root needs, since draft 4 has no `const` to discriminate on.
    async fn condition_holds(
        &self,
        root_url: &Url,
        condition: &Value,
        instance: &Value,
    ) -> Option<bool> {
        if instance.is_null() {
            return None;
        }

        let resolved = self.ref_schema_value(root_url, condition).await;
        let condition = resolved.as_deref().unwrap_or(condition);

        if names_a_ref(condition) {
            return None;
        }

        match self.create_validator(condition) {
            Ok(validator) => Some(validator.is_valid(instance)),
            Err(error) => {
                tracing::debug!(%error, "condition could not be compiled");
                None
            }
        }
    }

    /// The subschemas that apply to the same instance as `schema` itself and
    /// consume no path: the `if` branch the instance selects, and the schemas
    /// its present keys depend on.
    ///
    /// An absent instance, or a condition that cannot be decided, yields every
    /// branch, which is what traversal already offers for `oneOf` and `anyOf`.
    async fn conditional_subschemas<'s>(
        &self,
        root_url: &Url,
        schema: &'s Value,
        instance: &Value,
    ) -> Vec<&'s Value> {
        let mut applicable = Vec::new();

        let branches = [&schema["then"], &schema["else"]];

        if !schema["if"].is_null() && branches.iter().any(|branch| !branch.is_null()) {
            let selected: &[&Value] = match self
                .condition_holds(root_url, &schema["if"], instance)
                .await
            {
                Some(true) => &branches[..1],
                Some(false) => &branches[1..],
                None => &branches,
            };

            applicable.extend(selected.iter().copied().filter(|b| !b.is_null()));
        }

        applicable
    }
```

`conditional_subschemas` grows its dependent-schema half in Task 4; leave the `dependencies` and `dependentSchemas` keywords alone here.

- [ ] **Step 5: Read the branches in `collect_schemas`**

Immediately after the `allOf` loop and before `let include_self = …`:

```rust
        for conditional in self.conditional_subschemas(root_url, schema, value).await {
            self.collect_schemas(
                root_url,
                conditional,
                value,
                full_path.clone(),
                path,
                composition_depth,
                schemas,
            )
            .await?;
        }
```

In the `KeyOrIndex::Key` arm, change the array-of-tables descent's instance from `value` to `&value[k.value()]`, so every property descent forwards the same thing:

```rust
                // For array of tables.
                self.collect_schemas(
                    root_url,
                    &schema["items"][k.value()],
                    &value[k.value()],
                    full_path.join(k.clone()),
                    &child_path,
                    MAX_COMPOSITION_DEPTH,
                    schemas,
                )
                .await?;
```

- [ ] **Step 6: Thread the instance through `collect_child_schemas`**

In `possible_schemas_from`, replace the loop body:

```rust
        for (path, schema) in schemas {
            self.collect_child_schemas(
                schema_url,
                &schema,
                &path,
                &Keys::empty(),
                instance_at(value, &path),
                max_depth,
                MAX_COMPOSITION_DEPTH,
                &mut children,
            )
            .await;
        }
```

Add the parameter to `collect_child_schemas`, after `path`:

```rust
    async fn collect_child_schemas(
        &self,
        root_url: &Url,
        schema: &Value,
        root_path: &Keys,
        path: &Keys,
        instance: &Value,
        mut depth: usize,
        composition_depth: usize,
        schemas: &mut Vec<(Keys, Keys, Arc<Value>)>,
    ) {
```

Pass `instance` unchanged in the `$ref` tail call, the `oneOf` loop, the `anyOf` loop and the composed-`allOf` recursion. In the `properties` loop at the bottom, pass `&instance[k]`. Add the conditional descent immediately after the `anyOf` loop:

```rust
        for conditional in self.conditional_subschemas(root_url, schema, instance).await {
            self.collect_child_schemas(
                root_url,
                conditional,
                root_path,
                path,
                instance,
                depth,
                composition_depth,
                schemas,
            )
            .await;
        }
```

- [ ] **Step 7: Run the tests and watch them pass**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-common --features schema,reqwest,rustls-tls`

Expected: 27 passed, 0 failed.

If `an_absent_instance_takes_both_branches` reports `["then"]` only, the `Value::Null` guard in `condition_holds` is missing. If `the_schema_carrying_a_condition_is_still_collected` reports the two descriptions in the other order, the conditional loop was placed after the `include_self` push rather than before it; the expected order is branch first because the branch recursion runs first.

- [ ] **Step 8: Check the workspace**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo check --workspace --all-targets && cargo test --workspace 2>&1 | rg "^test result: FAILED"; echo "rg exit $?"`

Expected: check clean, `rg exit 1`.

- [ ] **Step 9: Commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-common/src/schema/mod.rs /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators add crates/taplo-common/src/schema/mod.rs crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators commit -m "feat(schema): pick the applicable if/then/else branch" -m "Traversal walked schema shape without ever consulting the document, so a
schema that routes its keys through then or else offered nothing at all.

The instance was already threaded through collect_schemas in step with
the path; the condition now reads it. An absent instance and a condition
that cannot be decided both fall back to every branch, which is what
oneOf and anyOf already offer. A condition compiled outside its document
loses the pointers its references name and rejects every instance, so a
condition that is a reference is resolved and one that holds a reference
is treated as undecided rather than false.

collect_child_schemas takes the instance too, so that it and
collect_schemas cannot disagree about the branch at the position they
share.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 3: Resolve document links against the whole document

`document_link` serializes each DOM *node* and passes it to `schemas_at_path` together with that node's full path, so traversal indexes a node by its own path and reads `Value::Null` at every position below the root. Harmless until Task 2; now it makes every condition undecidable in the links handler. This task also builds the two LSP test fixtures the handler-level criteria need.

**Files:**
- Modify: `crates/taplo-lsp/src/handlers/links.rs`
- Modify: `crates/taplo-lsp/src/handlers/hover.rs` (`mod tests` only: `hover_at_line`, `complete_at_line`, `links_at`)
- Test: `crates/taplo-lsp/src/handlers/hover.rs`, `crates/taplo-lsp/src/handlers/completion.rs`

**Interfaces:**
- Consumes: `world_with(schema, source) -> (Arc<WorldState<NativeEnvironment>>, Url)`, `hover_at`, `complete_at` — all already in `hover.rs`'s `mod tests`.
- Produces:
  - `pub(crate) async fn hover_at_line(schema, source, line: u32, character: u32) -> Option<String>`
  - `pub(crate) async fn complete_at_line(schema, source, line: u32, character: u32) -> Vec<CompletionItem>`
  - `pub(crate) async fn links_at(schema, source) -> Vec<lsp_types::DocumentLink>`

  `hover_at` and `complete_at` keep their signatures and delegate with `line` `0`, so none of the 59 existing callers changes.

- [ ] **Step 1: Write the failing tests**

In `crates/taplo-lsp/src/handlers/hover.rs`, inside `mod tests`, add:

```rust
    #[tokio::test]
    async fn hover_reads_the_branch_the_document_selects() {
        let schema = json!({
            "type": "object",
            "properties": { "kind": { "type": "string" } },
            "if": { "properties": { "kind": { "const": "docker" } }, "required": ["kind"] },
            "then": { "properties": { "image": { "description": "the image to pull" } } },
            "else": { "properties": { "image": { "description": "the binary to run" } } }
        });

        let hovered = hover_at_line(schema, "kind = \"docker\"\nimage = \"nginx\"\n", 1, 1).await;

        assert_eq!(hovered.as_deref(), Some("the image to pull"));
    }

    #[tokio::test]
    async fn links_follow_the_branch_the_document_selects() {
        let schema = json!({
            "type": "object",
            "properties": { "kind": { "type": "string" } },
            "if": { "properties": { "kind": { "const": "docker" } }, "required": ["kind"] },
            "then": {
                "properties": {
                    "image": { "x-taplo": { "links": { "key": "https://example.com/image" } } }
                }
            },
            "else": {
                "properties": {
                    "image": { "x-taplo": { "links": { "key": "https://example.com/binary" } } }
                }
            }
        });

        let links = links_at(schema, "kind = \"docker\"\nimage = \"nginx\"\n").await;

        let targets: Vec<String> = links
            .iter()
            .filter_map(|link| link.target.as_ref().map(ToString::to_string))
            .collect();

        assert_eq!(targets, ["https://example.com/image"]);
    }
```

In `crates/taplo-lsp/src/handlers/completion.rs`, inside `mod tests`, add:

```rust
    #[tokio::test]
    async fn key_completion_offers_only_the_selected_branch() {
        let schema = json!({
            "type": "object",
            "properties": { "kind": { "type": "string" } },
            "if": { "properties": { "kind": { "const": "docker" } }, "required": ["kind"] },
            "then": { "properties": { "image": { "type": "string" } } },
            "else": { "properties": { "binary": { "type": "string" } } }
        });

        let items = complete_at_line(schema, "kind = \"docker\"\ni\n", 1, 1).await;

        assert!(labels(&items).contains(&"image"));
        assert!(!labels(&items).contains(&"binary"));
    }
```

and extend its `use` line to `use super::super::hover::tests::{complete_at, complete_at_line};`.

- [ ] **Step 2: Run them and watch them fail**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-lsp --lib handlers 2>&1 | tail -30`

Expected: compile errors — `cannot find function hover_at_line`, `complete_at_line`, `links_at`. That is the right failure: the fixtures do not exist yet.

- [ ] **Step 3: Add the fixtures**

In `crates/taplo-lsp/src/handlers/hover.rs`'s `mod tests`, rewrite `hover_at` and `complete_at` as thin wrappers and add `links_at`:

```rust
    /// Returns the markdown the hover handler produces at a position on line 0.
    pub(crate) async fn hover_at(
        schema: serde_json::Value,
        source: &str,
        character: u32,
    ) -> Option<String> {
        hover_at_line(schema, source, 0, character).await
    }

    /// Returns the markdown the hover handler produces at a position.
    ///
    /// A conditional schema needs the discriminator and the key it selects on
    /// different lines, which is why the line is a parameter.
    pub(crate) async fn hover_at_line(
        schema: serde_json::Value,
        source: &str,
        line: u32,
        character: u32,
    ) -> Option<String> {
        let (world, document_url) = world_with(schema, source).await;

        let hovered = hover(
            lsp_async_stub::Context::detached(world),
            Some(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: document_url },
                    position: LspPosition::new(line, character),
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

    /// Returns the completion items the handler produces at a position on line 0.
    pub(crate) async fn complete_at(
        schema: serde_json::Value,
        source: &str,
        character: u32,
    ) -> Vec<lsp_types::CompletionItem> {
        complete_at_line(schema, source, 0, character).await
    }

    /// Returns the completion items the handler produces at a position.
    pub(crate) async fn complete_at_line(
        schema: serde_json::Value,
        source: &str,
        line: u32,
        character: u32,
    ) -> Vec<lsp_types::CompletionItem> {
        let (world, document_url) = world_with(schema, source).await;

        let response = crate::handlers::completion(
            lsp_async_stub::Context::detached(world),
            Some(lsp_types::CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: document_url },
                    position: LspPosition::new(line, character),
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

    /// Returns the document links the handler produces for a whole document.
    ///
    /// `schema.links` is off by default, and the handler returns nothing
    /// without it.
    pub(crate) async fn links_at(
        schema: serde_json::Value,
        source: &str,
    ) -> Vec<lsp_types::DocumentLink> {
        let (world, document_url) = world_with(schema, source).await;

        {
            let mut workspaces = world.workspaces.write().await;
            workspaces.by_document_mut(&document_url).config.schema.links = true;
        }

        crate::handlers::links(
            lsp_async_stub::Context::detached(world),
            Some(lsp_types::DocumentLinkParams {
                text_document: TextDocumentIdentifier { uri: document_url },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .into(),
        )
        .await
        .unwrap()
        .unwrap_or_default()
    }
```

- [ ] **Step 4: Serialize the document once in `links.rs`**

In `crates/taplo-lsp/src/handlers/links.rs`, insert before the `for (keys, last_key, node) in …` loop:

```rust
    let document = match serde_json::to_value(&doc.dom) {
        Ok(v) => v,
        Err(error) => {
            tracing::warn!(%error, "cannot turn DOM into JSON");
            return Ok(None);
        }
    };
```

and delete the per-node block inside the loop:

```rust
            let value = match serde_json::to_value(&node) {
                Ok(v) => v,
                Err(error) => {
                    tracing::debug!(%error, "invalid TOML value");
                    continue;
                }
            };
```

changing the call that follows to pass `&document`. The loop no longer binds `node`, so change its destructuring to `for (keys, last_key, _) in …`, or drop the third tuple element from the `filter_map` if nothing else reads it — check with `rg -n "node" crates/taplo-lsp/src/handlers/links.rs` before editing.

- [ ] **Step 5: Run the tests and watch them pass**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-lsp --lib handlers`

Expected: all pass, including the 51 pre-existing `handlers::hover` tests and the 8 pre-existing `handlers::completion` tests.

If `links_follow_the_branch_the_document_selects` returns both targets, `links.rs` is still passing the node. If it returns none, `config.schema.links` was not set, or the `x-taplo` extension key was mistyped.

- [ ] **Step 6: Check the workspace**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo check --workspace --all-targets && cargo test --workspace 2>&1 | rg "^test result: FAILED"; echo "rg exit $?"`

Expected: check clean, `rg exit 1`.

- [ ] **Step 7: Commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-lsp/src/handlers/links.rs /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-lsp/src/handlers/hover.rs /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-lsp/src/handlers/completion.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators add crates/taplo-lsp/src/handlers/links.rs crates/taplo-lsp/src/handlers/hover.rs crates/taplo-lsp/src/handlers/completion.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators commit -m "fix(lsp): resolve links against the whole document" -m "The links handler serialized each DOM node and passed it to traversal
beside that node's full path, so traversal indexed a node by its own
path and read an absent instance everywhere below the root. Now that a
condition consults the instance, that made every condition undecided in
this handler alone, and attached links from branches the document had
ruled out. Serializing once before the loop is also fewer
serializations than one per node.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 4: Apply the schemas a present key depends on

**Files:**
- Modify: `crates/taplo-common/src/schema/mod.rs` (`conditional_subschemas`)
- Test: `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Consumes: `conditional_subschemas` from Task 2.
- Produces: no new signatures; `conditional_subschemas` additionally returns dependent subschemas.

- [ ] **Step 1: Write the failing tests**

Append to `crates/taplo-common/src/schema/tests.rs`:

```rust
/// One dependent schema under whichever keyword spells it, so that both
/// spellings are asserted against the same shape.
fn dependent_schema(keyword: &str) -> Value {
    json!({
        "type": "object",
        keyword: {
            "kind": { "properties": { "image": { "description": "dependent" } } }
        }
    })
}

#[tokio::test]
async fn a_present_trigger_key_applies_its_dependent_schema() {
    for keyword in ["dependencies", "dependentSchemas"] {
        let (schemas, url) = seeded(dependent_schema(keyword)).await;

        let keys = "image".parse::<Keys>().unwrap();
        let found = schemas
            .schemas_at_path(&url, &json!({ "kind": "docker" }), &keys)
            .await
            .unwrap();

        assert_eq!(descriptions(&found), ["dependent"], "keyword {keyword}");
    }
}

#[tokio::test]
async fn an_absent_trigger_key_applies_nothing() {
    for keyword in ["dependencies", "dependentSchemas"] {
        let (schemas, url) = seeded(dependent_schema(keyword)).await;

        let keys = "image".parse::<Keys>().unwrap();
        let found = schemas
            .schemas_at_path(&url, &json!({ "other": 1 }), &keys)
            .await
            .unwrap();

        assert!(found.is_empty(), "keyword {keyword}");
    }
}

#[tokio::test]
async fn an_absent_instance_applies_every_dependent_schema() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "dependentSchemas": {
            "a": { "properties": { "image": { "description": "from a" } } },
            "b": { "properties": { "image": { "description": "from b" } } }
        }
    }))
    .await;

    let keys = "image".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &Value::Null, &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["from a", "from b"]);
}

#[tokio::test]
async fn the_array_form_of_dependencies_applies_nothing() {
    for keyword in ["dependencies", "dependentRequired"] {
        let (schemas, url) = seeded(json!({
            "type": "object",
            keyword: { "kind": ["image"] }
        }))
        .await;

        let keys = "image".parse::<Keys>().unwrap();
        let found = schemas
            .schemas_at_path(&url, &json!({ "kind": "docker" }), &keys)
            .await
            .unwrap();

        assert!(found.is_empty(), "keyword {keyword}");
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-common --features schema,reqwest,rustls-tls dependen`

Expected: `an_absent_trigger_key_applies_nothing` and `the_array_form_of_dependencies_applies_nothing` pass — nothing descends yet, so nothing is found. The other two fail with `left: [], right: ["dependent"]` and `left: [], right: ["from a", "from b"]`.

- [ ] **Step 3: Read the dependent schemas**

In `conditional_subschemas`, before `applicable`'s return:

```rust
        // `dependentSchemas` is the 2019-09 spelling of `dependencies`' schema
        // form. Traversal reads whichever a schema happens to carry, the way
        // it reads `prefixItems` and tuple `items` side by side. The array
        // form of either names keys rather than a schema, and is skipped by
        // the object check.
        for keyword in ["dependencies", "dependentSchemas"] {
            let Some(dependents) = schema[keyword].as_object() else {
                continue;
            };

            for (trigger, dependent) in dependents {
                if !dependent.is_object() {
                    continue;
                }

                if instance.is_null() || instance.get(trigger).is_some() {
                    applicable.push(dependent);
                }
            }
        }
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-common --features schema,reqwest,rustls-tls`

Expected: 31 passed, 0 failed.

- [ ] **Step 5: Check the workspace**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo check --workspace --all-targets && cargo test --workspace 2>&1 | rg "^test result: FAILED"; echo "rg exit $?"`

Expected: check clean, `rg exit 1`.

- [ ] **Step 6: Commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-common/src/schema/mod.rs /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators add crates/taplo-common/src/schema/mod.rs crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators commit -m "feat(schema): apply schemas a present key depends on" -m "A dependent schema applies to the same object as the schema that names
it, when the trigger key is present, which is the question the if branch
already answers against the same instance. Both live in one helper.

The array form of dependencies, and all of dependentRequired, name keys
rather than a schema. There is nothing for traversal to yield, and they
keep validating.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 5: Fall back to `unevaluatedProperties`

`unevaluatedProperties` applies to a key no other applicator in the schema evaluated. `collect_schemas` walks exactly that set of applicators one line before the question is asked, so it answers on the way past: the function returns whether any applicator in its in-place closure evaluates the first segment of `path`.

**Files:**
- Modify: `crates/taplo-common/src/schema/mod.rs` (`schemas_at_path`, `collect_schemas`)
- Test: `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Consumes: everything from Tasks 1, 2 and 4.
- Produces: `collect_schemas` returns `Result<bool, anyhow::Error>` — whether this schema's in-place closure evaluates `path`'s first segment. `schemas_at_path` is its only outside caller and discards the value.

- [ ] **Step 1: Write the failing tests**

Append to `crates/taplo-common/src/schema/tests.rs`:

```rust
#[tokio::test]
async fn an_unevaluated_key_falls_back_to_unevaluated_properties() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": { "known": { "description": "known" } },
        "unevaluatedProperties": { "description": "unevaluated" }
    }))
    .await;

    let keys = "other".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "other": 1 }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["unevaluated"]);
}

#[tokio::test]
async fn an_evaluated_key_does_not_fall_back() {
    let cases = [
        json!({ "properties": { "known": { "description": "known" } } }),
        json!({ "patternProperties": { "^kn": { "description": "known" } } }),
        json!({ "additionalProperties": { "description": "known" } }),
        json!({ "allOf": [{ "properties": { "known": { "description": "known" } } }] }),
        json!({ "oneOf": [{ "properties": { "known": { "description": "known" } } }] }),
        json!({ "anyOf": [{ "properties": { "known": { "description": "known" } } }] }),
        json!({
            "if": { "properties": { "kind": { "const": "a" } }, "required": ["kind"] },
            "then": { "properties": { "known": { "description": "known" } } }
        }),
        json!({
            "dependentSchemas": {
                "kind": { "properties": { "known": { "description": "known" } } }
            }
        }),
    ];

    for case in cases {
        let mut schema = case.clone();
        schema["type"] = json!("object");
        schema["unevaluatedProperties"] = json!({ "description": "unevaluated" });

        let (schemas, url) = seeded(schema).await;

        let keys = "known".parse::<Keys>().unwrap();
        let found = schemas
            .schemas_at_path(&url, &json!({ "kind": "a", "known": 1 }), &keys)
            .await
            .unwrap();

        assert_eq!(descriptions(&found), ["known"], "case {case}");
    }
}

#[tokio::test]
async fn a_key_only_the_unselected_branch_evaluates_falls_back() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": { "properties": { "kind": { "const": "a" } }, "required": ["kind"] },
        "then": { "properties": { "other": { "description": "then" } } },
        "unevaluatedProperties": { "description": "unevaluated" }
    }))
    .await;

    let keys = "other".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "kind": "b", "other": 1 }), &keys)
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["unevaluated"]);
}

#[tokio::test]
async fn a_deep_path_asks_about_its_head_key() {
    let cases = [
        json!({ "allOf": [{ "properties": { "a": { "type": "object" } } }] }),
        json!({ "additionalProperties": false }),
    ];

    for case in cases {
        let mut schema = case.clone();
        schema["type"] = json!("object");
        schema["unevaluatedProperties"] =
            json!({ "properties": { "b": { "description": "unevaluated" } } });

        let (schemas, url) = seeded(schema).await;

        let keys = "a.b".parse::<Keys>().unwrap();
        let found = schemas
            .schemas_at_path(&url, &json!({ "a": { "b": 1 } }), &keys)
            .await
            .unwrap();

        assert!(found.is_empty(), "case {case}");
    }
}

#[tokio::test]
async fn a_boolean_unevaluated_properties_yields_nothing() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": { "known": { "description": "known" } },
        "unevaluatedProperties": false
    }))
    .await;

    let keys = "other".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "other": 1 }), &keys)
        .await
        .unwrap();

    assert!(found.is_empty());
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-common --features schema,reqwest,rustls-tls unevaluated`

Expected: `an_evaluated_key_does_not_fall_back`, `a_deep_path_asks_about_its_head_key` and `a_boolean_unevaluated_properties_yields_nothing` pass — nothing descends into `unevaluatedProperties` yet. `an_unevaluated_key_falls_back_to_unevaluated_properties` and `a_key_only_the_unselected_branch_evaluates_falls_back` fail with `left: [], right: ["unevaluated"]`.

- [ ] **Step 3: Return the coverage answer from `collect_schemas`**

Change the return type to `Result<bool, anyhow::Error>` and the early return to `Ok(false)`:

```rust
    ) -> Result<bool, anyhow::Error> {
        if !schema.is_object() || composition_depth == 0 {
            return Ok(false);
        }
```

The `$ref` tail call already returns what the target returns, so it needs no change beyond its type. Accumulate the in-place answers:

```rust
        let mut evaluated = false;

        if let Some(one_ofs) = schema["oneOf"].as_array() {
            for one_of in one_ofs {
                evaluated |= self
                    .collect_schemas(
                        root_url,
                        one_of,
                        value,
                        full_path.clone(),
                        path,
                        composition_depth,
                        schemas,
                    )
                    .await?;
            }
        }
```

and the same for `anyOf`, `allOf` and the `conditional_subschemas` loop.

The empty-path arm answers `false`, because there is no key to have evaluated:

```rust
        let Some(key) = path.iter().next() else {
            if include_self {
                schemas.push((full_path.clone(), Arc::new(schema.clone())));
            }
            return Ok(false);
        };
```

In the `KeyOrIndex::Key` arm, discard what the path-consuming descents return and record what this schema itself covers. The array-of-tables descent covers no property, so its answer is discarded and it contributes nothing:

```rust
                let _ = self
                    .collect_schemas(
                        root_url,
                        &schema["properties"][k.value()],
                        &value[k.value()],
                        full_path.join(k.clone()),
                        &child_path,
                        MAX_COMPOSITION_DEPTH,
                        schemas,
                    )
                    .await?;
                evaluated |= !schema["properties"][k.value()].is_null();
```

`additionalProperties` counts whenever it is written in any form, `false` included, because it then evaluates every key the named properties do not:

```rust
                evaluated |= !schema["additionalProperties"].is_null();
```

and a matching `patternProperties` pattern sets `evaluated = true;` inside the `if re.is_match(k.value())` block.

After the `patternProperties` loop, and still inside the `KeyOrIndex::Key` arm:

```rust
                // `unevaluatedProperties` applies to a key no other applicator
                // in this schema evaluated, which is the question every
                // in-place recursion above has just answered for this key.
                if !evaluated {
                    let _ = self
                        .collect_schemas(
                            root_url,
                            &schema["unevaluatedProperties"],
                            &value[k.value()],
                            full_path.join(k.clone()),
                            &child_path,
                            MAX_COMPOSITION_DEPTH,
                            schemas,
                        )
                        .await?;
                }
```

The `KeyOrIndex::Index` arm discards its descent's answer and leaves `evaluated` alone; an index is not a property. End the function with `Ok(evaluated)`.

Finally, in `schemas_at_path`, bind the value:

```rust
        let _evaluated = self
            .collect_schemas(
                schema_url,
                &schema,
                value,
                Keys::empty(),
                path,
                MAX_COMPOSITION_DEPTH,
                &mut schemas,
            )
            .await?;
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-common --features schema,reqwest,rustls-tls`

Expected: 36 passed, 0 failed.

If `a_deep_path_asks_about_its_head_key` fails, `evaluated` is being read from a path-consuming descent's return value instead of from this schema's own keywords.

- [ ] **Step 5: Check the workspace**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo check --workspace --all-targets && cargo test --workspace 2>&1 | rg "^test result: FAILED"; echo "rg exit $?"`

Expected: check clean, `rg exit 1`.

- [ ] **Step 6: Commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-common/src/schema/mod.rs /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators add crates/taplo-common/src/schema/mod.rs crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators commit -m "feat(schema): fall back to unevaluatedProperties" -m "A key that properties, patternProperties, additionalProperties and every
in-place applicator leave alone is what unevaluatedProperties describes,
and traversal walked all of them one step before the question mattered.
collect_schemas now returns whether its in-place closure evaluated the
first segment of the path, and descends into unevaluatedProperties only
when the answer is no.

Counting pushes into the accumulator instead would have asked a
different question: a push only happens at the target path, so an empty
accumulator means no route reached the target rather than that no
applicator evaluated the key.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 6: Extract `constraint_facts` from `key_hover_sections`

A pure refactor, so that the commit after it is only behavior. The constraint block moves out of `key_hover_sections` unchanged, ready for a second caller.

**Files:**
- Modify: `crates/taplo-lsp/src/handlers/hover.rs`

**Interfaces:**
- Produces: `fn constraint_facts(schema: &Value) -> Vec<Fact>`

- [ ] **Step 1: Move the block**

In `crates/taplo-lsp/src/handlers/hover.rs`, add above `key_hover_sections`:

```rust
/// The constraint keywords a schema states about its own value, in the order
/// hover lists them.
///
/// Each group is filtered by the type its keywords constrain, so a keyword
/// written against a type the schema does not admit is vacuous and left out.
fn constraint_facts(schema: &Value) -> Vec<Fact> {
    let mut facts = Vec::new();

    if admits_type(schema, "number") {
        facts.extend(range_fact(schema));
        facts.extend(multiple_of_fact(schema));
    }

    if admits_type(schema, "string") {
        facts.extend(bounds_fact(
            "Length",
            inclusive_bounds(schema, "minLength"),
            inclusive_bounds(schema, "maxLength"),
        ));
        facts.extend(string_fact(schema, "Pattern", "pattern"));
        facts.extend(string_fact(schema, "Format", "format"));
        facts.extend(string_fact(schema, "Media type", "contentMediaType"));
        facts.extend(string_fact(schema, "Encoding", "contentEncoding"));
    }

    if admits_type(schema, "array") {
        facts.extend(bounds_fact(
            "Items",
            inclusive_bounds(schema, "minItems"),
            inclusive_bounds(schema, "maxItems"),
        ));

        if flag(schema, "uniqueItems") {
            facts.push(Fact {
                label: "Unique items",
                values: Vec::new(),
            });
        }
    }

    if admits_type(schema, "object") {
        facts.extend(bounds_fact(
            "Properties",
            inclusive_bounds(schema, "minProperties"),
            inclusive_bounds(schema, "maxProperties"),
        ));
    }

    facts
}
```

In `key_hover_sections`, replace the four `if admits_type(…)` blocks — everything between `sections.facts.extend(examples_fact(schema));` and `if flag(schema, "readOnly")` — with:

```rust
    sections.facts.extend(constraint_facts(schema));
```

- [ ] **Step 2: Run the tests and watch nothing change**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-lsp --lib handlers::hover`

Expected: 51 passed, 0 failed. Any failure means the block was reordered or a call was dropped in the move.

- [ ] **Step 3: Commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-lsp/src/handlers/hover.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators add crates/taplo-lsp/src/handlers/hover.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators commit -m "refactor(lsp): extract constraint_facts" -m "Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 7: Render `not`, `propertyNames` and `contains`

None of the three describes the value at a child position, so traversal never descends into any of them. Hover states each at the schema that writes it, labelled, so a prohibition is never read as a requirement.

**Files:**
- Modify: `crates/taplo-lsp/src/handlers/hover.rs`
- Test: `crates/taplo-lsp/src/handlers/hover.rs`, `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Consumes: `constraint_facts` from Task 6; `Fact`, `HoverSections`, `code_span`, `admits_type` as they stand.
- Produces:
  - `struct NestedFacts { label: &'static str, facts: Vec<Fact> }` and `HoverSections::nested: Vec<NestedFacts>`
  - `fn fact_line(fact: &Fact, indent: &str) -> String`
  - `fn subschema_facts(schema: &Value) -> Option<Vec<Fact>>`
  - `fn type_fact`, `fn const_fact`, `fn enum_fact`, `fn required_fact`, each `(schema: &Value) -> Option<Fact>`
  - `const RENDERABLE_SUBSCHEMA_KEYWORDS: &[&str]`

- [ ] **Step 1: Write the failing traversal tests**

Append to `crates/taplo-common/src/schema/tests.rs`:

```rust
#[tokio::test]
async fn a_negated_subschema_is_never_traversed() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "not": { "properties": { "banned": { "description": "banned" } } }
    }))
    .await;

    let keys = "banned".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "banned": 1 }), &keys)
        .await
        .unwrap();
    assert!(found.is_empty());

    let children = schemas
        .possible_schemas_from(&url, &json!({}), &Keys::empty(), 5)
        .await
        .unwrap();
    assert!(children
        .iter()
        .all(|(_, relative, _)| relative.to_string() != "banned"));
}

#[tokio::test]
async fn property_names_is_never_the_schema_for_a_value() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "propertyNames": { "pattern": "^[a-z]+$", "description": "names" }
    }))
    .await;

    let keys = "abc".parse::<Keys>().unwrap();
    let found = schemas
        .schemas_at_path(&url, &json!({ "abc": 1 }), &keys)
        .await
        .unwrap();

    assert!(found.is_empty());
}

#[tokio::test]
async fn contains_is_never_the_schema_for_an_index() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "list": {
                "type": "array",
                "contains": { "type": "string", "description": "contained" }
            }
        }
    }))
    .await;

    let keys = "list".parse::<Keys>().unwrap().join(0_usize);
    let found = schemas
        .schemas_at_path(&url, &json!({ "list": ["x"] }), &keys)
        .await
        .unwrap();

    assert!(found.is_empty());
}
```

- [ ] **Step 2: Write the failing hover tests**

In `crates/taplo-lsp/src/handlers/hover.rs`'s `mod tests`:

```rust
    #[tokio::test]
    async fn renders_a_prohibition_as_a_labelled_block() {
        let schema = json!({
            "type": "object",
            "properties": {
                "tag": {
                    "type": "string",
                    "not": { "type": "string", "pattern": "^latest$" }
                }
            }
        });

        let hovered = hover_at(schema, "tag = \"v1\"\n", 1).await;

        assert_eq!(
            hovered.as_deref(),
            Some("- Must not match\n  - Type: `string`\n  - Pattern: `^latest$`")
        );
    }

    #[tokio::test]
    async fn renders_key_names_and_contains_as_labelled_blocks() {
        let schema = json!({
            "type": "object",
            "properties": {
                "env": {
                    "type": "object",
                    "minProperties": 1,
                    "propertyNames": { "pattern": "^[A-Z_]+$" }
                },
                "tags": {
                    "type": "array",
                    "contains": { "const": "release" }
                }
            }
        });

        assert_eq!(
            hover_at(schema.clone(), "env = {}\n", 1).await.as_deref(),
            Some("- Properties: `>= 1`\n- Key names\n  - Pattern: `^[A-Z_]+$`")
        );
        assert_eq!(
            hover_at(schema, "tags = []\n", 1).await.as_deref(),
            Some("- Contains\n  - Const: `\"release\"`")
        );
    }

    #[tokio::test]
    async fn a_subschema_with_nothing_to_show_produces_no_block() {
        let cases = [
            json!({ "not": {} }),
            json!({ "not": { "properties": { "a": { "const": 1 } }, "required": ["a"] } }),
            json!({ "not": { "dependentRequired": { "a": ["b"] } , "required": ["a"] } }),
        ];

        for case in cases {
            let mut property = case.clone();
            property["type"] = json!("string");

            let schema = json!({
                "type": "object",
                "properties": { "tag": property }
            });

            assert_eq!(hover_at(schema, "tag = \"v1\"\n", 1).await, None, "case {case}");
        }
    }

    #[tokio::test]
    async fn a_prohibition_states_what_a_subschema_requires() {
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": {
                    "not": { "enum": ["debug", "trace"] }
                }
            }
        });

        let hovered = hover_at(schema, "mode = \"fast\"\n", 1).await;

        assert_eq!(
            hovered.as_deref(),
            Some("- Must not match\n  - One of: `\"debug\"`, `\"trace\"`")
        );
    }
```

- [ ] **Step 3: Run them and watch them fail**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-common --features schema,reqwest,rustls-tls && cargo test -p taplo-lsp --lib handlers::hover`

Expected: the three traversal tests pass already — traversal never descended into any of the three keywords and still does not; they are the regression guard that it stays that way. The four hover tests fail, three with `left: None, right: Some("- Must not match…")` and `a_subschema_with_nothing_to_show_produces_no_block` passing vacuously.

- [ ] **Step 4: Add the nested section**

In `crates/taplo-lsp/src/handlers/hover.rs`, beside `Fact`:

```rust
/// A subschema a schema names in a role that is not "the value here": a
/// prohibition, a rule for key names, a rule some element must satisfy.
struct NestedFacts {
    label: &'static str,
    facts: Vec<Fact>,
}
```

Add the field to `HoverSections`:

```rust
    /// One labelled block per subschema whose role is not "the value here",
    /// rendered beneath the flat facts.
    nested: Vec<NestedFacts>,
```

Extract the per-fact line and rewrite the fact block of `render`:

```rust
/// One bullet for a fact, indented for a nested block.
fn fact_line(fact: &Fact, indent: &str) -> String {
    if fact.values.is_empty() {
        format!("{indent}- {}", fact.label)
    } else {
        format!(
            "{indent}- {}: {}",
            fact.label,
            fact.values.iter().map(|value| code_span(value)).join(", ")
        )
    }
}
```

```rust
        if !self.facts.is_empty() || !self.nested.is_empty() {
            let mut lines: Vec<String> =
                self.facts.iter().map(|fact| fact_line(fact, "")).collect();

            for nested in &self.nested {
                lines.push(format!("- {}", nested.label));
                lines.extend(nested.facts.iter().map(|fact| fact_line(fact, "  ")));
            }

            blocks.push(lines.join("\n"));
        }
```

- [ ] **Step 5: Add the subschema contributors**

```rust
/// Keywords a nested block can show: the ones `subschema_facts` renders, and
/// annotations that constrain nothing.
///
/// An allowlist rather than a list of what to refuse, because the two go stale
/// in opposite directions. `subschema_facts` is one level deep, and an
/// incomplete requirement is merely incomplete where an incomplete prohibition
/// is wrong: a `not` over `properties` and `required` together forbids one
/// value of one key, and a block naming only the key would read as forbidding
/// the key. A keyword nobody has heard of yet costs a missing block.
const RENDERABLE_SUBSCHEMA_KEYWORDS: &[&str] = &[
    "type",
    "const",
    "enum",
    "required",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "contentMediaType",
    "contentEncoding",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
    "title",
    "description",
    "markdownDescription",
    "$comment",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
    "$id",
    "$schema",
    "definitions",
    "$defs",
    "x-taplo",
];

/// The types a subschema admits.
fn type_fact(schema: &Value) -> Option<Fact> {
    let values: Vec<String> = match &schema["type"] {
        Value::String(name) => vec![name.clone()],
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    };

    (!values.is_empty()).then_some(Fact {
        label: "Type",
        values,
    })
}

/// The single value a subschema admits.
fn const_fact(schema: &Value) -> Option<Fact> {
    let node: Node = serde_json::from_value(schema.get("const")?.clone()).ok()?;

    Some(Fact {
        label: "Const",
        values: vec![node.to_toml(true, false)],
    })
}

/// The values a subschema admits, as TOML literals.
fn enum_fact(schema: &Value) -> Option<Fact> {
    let values: Vec<String> = schema["enum"]
        .as_array()?
        .iter()
        .filter_map(|value| serde_json::from_value::<Node>(value.clone()).ok())
        .map(|node| node.to_toml(true, false))
        .collect();

    (!values.is_empty()).then_some(Fact {
        label: "One of",
        values,
    })
}

/// The keys a subschema demands.
fn required_fact(schema: &Value) -> Option<Fact> {
    let values: Vec<String> = schema["required"]
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect();

    (!values.is_empty()).then_some(Fact {
        label: "Required",
        values,
    })
}

/// What a subschema states about itself, for a reader who has been told the
/// role it plays, or nothing when the block would be incomplete.
///
/// `type`, `const`, `enum` and `required` appear here and not among a schema's
/// own facts because a nested block has no other channel: at the top level the
/// written value shows its type, and `enum` and `const` reach the reader
/// through value completion and value hover.
fn subschema_facts(schema: &Value) -> Option<Vec<Fact>> {
    let keywords = schema.as_object()?;

    if !keywords
        .keys()
        .all(|keyword| RENDERABLE_SUBSCHEMA_KEYWORDS.contains(&keyword.as_str()))
    {
        return None;
    }

    let mut facts = Vec::new();

    facts.extend(type_fact(schema));
    facts.extend(const_fact(schema));
    facts.extend(enum_fact(schema));
    facts.extend(required_fact(schema));
    facts.extend(constraint_facts(schema));

    (!facts.is_empty()).then_some(facts)
}
```

At the end of `key_hover_sections`, after the `writeOnly` block and before `sections`:

```rust
    for (label, keyword) in [
        ("Must not match", "not"),
        ("Key names", "propertyNames"),
        ("Contains", "contains"),
    ] {
        if let Some(facts) = subschema_facts(&schema[keyword]) {
            sections.nested.push(NestedFacts { label, facts });
        }
    }
```

- [ ] **Step 6: Run the tests and watch them pass**

Run: `cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-lsp --lib handlers::hover`

Expected: 57 passed, 0 failed — 51 from the branch point, two from Task 3, four from this task.

If `renders_a_prohibition_as_a_labelled_block` reports a blank line between the flat list and the block, the nested loop is pushing a separate block into `blocks` instead of extending `lines`.

- [ ] **Step 7: Check the workspace and the wasm target**

Run:

```bash
cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo check --workspace --all-targets && cargo test --workspace 2>&1 | rg "^test result: FAILED"; echo "rg exit $?"
cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo check --target wasm32-unknown-unknown --manifest-path crates/taplo-wasm/Cargo.toml
```

Expected: check clean, `rg exit 1`, wasm check finishes.

- [ ] **Step 8: Commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-lsp/src/handlers/hover.rs /home/lev/Git/lev/taplo-wt/schema-applicators/crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators add crates/taplo-lsp/src/handlers/hover.rs crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-applicators commit -m "feat(lsp): render not, propertyNames and contains" -m "None of the three describes the value at a child position: a prohibition
forbids the whole instance from matching, propertyNames constrains key
names, and contains asks only that some element match. Traversal
descends into none of them, and tests hold it there, because yielding a
subschema is read everywhere as a requirement on the value at the
cursor.

Hover states each at the schema that writes it, as a labelled block of
the subschema's own facts. A subschema is shown only when every keyword
it carries is one the block can render, since a partial requirement is
incomplete but a partial prohibition is wrong.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Final verification

After Task 7, with a clean tree:

```bash
cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo check --workspace --all-targets
cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test --workspace
cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-common --features schema,reqwest,rustls-tls
cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo test -p taplo-lsp --lib handlers::hover
cd /home/lev/Git/lev/taplo-wt/schema-applicators && cargo check --target wasm32-unknown-unknown --manifest-path crates/taplo-wasm/Cargo.toml
git -C /home/lev/Git/lev/taplo-wt/schema-applicators status --short
```

Expected: every command clean, `git status` empty. `cargo fmt --check` is *not* expected to be clean; it was not clean at the branch point either.
