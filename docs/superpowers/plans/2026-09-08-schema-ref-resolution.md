# JSON Schema `$ref` resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make traversal resolve a `$ref` the way RFC 3986 and `$id` say it should, apply the keywords written beside one, decide a condition that holds one, and stop a reference cycle from hanging the language server.

**Architecture:** One base URL travels with the schema. `Url::join` replaces `reference_url`, `$id` moves the base, and `resolve_schema` returns the base it arrived at beside the value. A `$ref` carrying other keys merges its target underneath them instead of discarding them. A set of resolved `$ref` URLs, pushed and popped along each in-place chain, bounds the work a cycle can do. On the validation side one change — making the root `$id` absolute against the URL the schema was loaded from — starts `jsonschema`'s own resolver where traversal starts, so the two agree.

**Tech Stack:** Rust, `serde_json`, `url`, `percent-encoding`, `jsonschema` 0.17.1, `async_recursion`, `lsp-types`, `tokio` tests.

**Spec:** `docs/superpowers/specs/2026-09-08-schema-ref-resolution.md`

## Global Constraints

- Work only in the worktree `/home/lev/Git/lev/taplo-wt/schema-ref-resolution`, on branch `feat/schema-ref-resolution`. Use `git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution …` and absolute paths in every command. Never `cd` first. Never `git stash`; read another revision with `git show REV:path`.
- Commit, do not push. No `git push`, no pull request, no issue edits.
- Conventional Commits, imperative mood, lowercase after the colon, subject ≤ 50 characters. A body only where the change needs context. End every commit message with `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.
- Comments are timeless: what the code does and the non-obvious why. Never "this PR", "now we", "previously", never a task or issue number.
- `rg` not `grep`, `fd` not `find`. `rg -r` means `--replace`, so line numbers are `rg -n`.
- `cargo fmt --check` is not clean at the branch point. Format only the files you touch: run `rustfmt --edition 2021 <file>` on the exact files changed.
- Two `dead_code` warnings, in `taplo` and `lsp-async-stub`, are inherited and expected.
- Run `taplo-common`'s schema tests with `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls`. `cargo test --workspace` reaches them through feature unification and stops at the first failing crate, so a failure there hides later crates' results.
- Every timing test runs its traversal through `assert_finishes_within` (Task 1), on its own thread, with a bound well under 60 s. `tokio::time::timeout` cannot stop a traversal that never yields — it has no await that reaches the runtime — and at 60 s the in-memory schema LRU expires (`DEFAULT_LRU_CACHE_EXPIRATION_TIME`), after which a seeded test falls through to a file read that fails and reports the wrong failure.
- Where a step says "PASS", it means every test that passed before the task still passes, plus the ones the task adds. Do not treat a total count as the assertion.
- The baseline at `dc31977` is green: `cargo check --workspace --all-targets`, `cargo test --workspace`, `cargo test -p taplo-common --features schema,reqwest,rustls-tls` (39 tests), `cargo test -p taplo-lsp --lib handlers::hover` (57 tests), and `cargo check --target wasm32-unknown-unknown --manifest-path crates/taplo-wasm/Cargo.toml`.

---

### Task 1: Bound the work a reference cycle can do

`MAX_COMPOSITION_DEPTH` bounds how *deep* composition may go, not how much work it may do. A `$ref` hop costs a unit and its target costs another, so a cycle gets sixteen rounds and a branching cycle costs its fan-out to that power. Measured today: an `anyOf` of three references back to itself takes 63 s in `schemas_at_path`; a composed `allOf` of three does not finish in 400 s. This lands first because every task after it adds tests over cyclic fixtures.

**Files:**
- Modify: `crates/taplo-common/src/schema/mod.rs` (`schemas_at_path`, `collect_schemas`, `possible_schemas_from`, `collect_child_schemas`, `ref_schema_value`, `condition_holds`)
- Test: `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Produces: `collect_schemas(&self, root_url, schema, value, full_path, path, composition_depth, visited: &mut Vec<Url>, schemas)` and `collect_child_schemas(&self, root_url, schema, root_path, path, instance, depth, composition_depth, visited: &mut Vec<Url>, schemas)` — one new parameter before `schemas`.
- Produces: `ref_schema_value(&self, root_url: &Url, schema: &Value) -> Option<(Url, Arc<Value>)>` — the resolved URL beside the value, because the caller needs it for the visited set.

- [ ] **Step 1: Write the failing tests**

Append to `crates/taplo-common/src/schema/tests.rs`:

```rust
/// Runs an async traversal on its own thread and runtime and panics with
/// `what` when it has not finished inside `bound`.
///
/// `tokio::time::timeout` cannot bound a traversal: it never reaches an await
/// point that yields to the runtime, so the timeout future never gets to run.
fn assert_finishes_within<T, F, Fut>(
    bound: std::time::Duration,
    what: &'static str,
    work: F,
) -> T
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T>,
{
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        drop(tx.send(runtime.block_on(work())));
    });

    rx.recv_timeout(bound).unwrap_or_else(|_| panic!("{what}"))
}

/// The composed-`allOf` branch merges its members instead of recursing on
/// `$ref`, so a cycle whose nodes are reachable only as members is invisible to
/// a check that consults the visited set without ever feeding it.
#[test]
fn a_cycle_through_composed_all_of_members_terminates() {
    assert_finishes_within(
        std::time::Duration::from_secs(5),
        "a cycle through merged allOf members did not terminate",
        || async {
            let (schemas, url) = seeded(json!({
                "type": "object",
                "properties": { "a": { "$ref": "#/definitions/a" } },
                "definitions": {
                    "a": { "allOf": [{ "$ref": "#/definitions/b" }] },
                    "b": { "allOf": [
                        { "$ref": "#/definitions/c" },
                        { "$ref": "#/definitions/d" },
                        { "$ref": "#/definitions/e" }
                    ] },
                    "c": { "allOf": [{ "$ref": "#/definitions/b" }] },
                    "d": { "allOf": [{ "$ref": "#/definitions/b" }] },
                    "e": { "allOf": [{ "$ref": "#/definitions/b" }] }
                }
            }))
            .await;

            schemas
                .possible_schemas_from(&url, &Value::Null, &Keys::empty(), 5)
                .await
                .map(|found| found.len())
        },
    )
    .unwrap();
}

#[test]
fn a_composed_all_of_cycle_terminates() {
    assert_finishes_within(
        std::time::Duration::from_secs(5),
        "a composed allOf cycle did not terminate",
        || async {
            let (schemas, url) = seeded(json!({
                "type": "object",
                "properties": { "node": { "$ref": "#/definitions/node" } },
                "definitions": {
                    "node": { "allOf": [
                        { "$ref": "#/definitions/node" },
                        { "$ref": "#/definitions/node" },
                        { "$ref": "#/definitions/node" }
                    ] }
                }
            }))
            .await;

            schemas
                .possible_schemas_from(&url, &Value::Null, &Keys::empty(), 5)
                .await
                .map(|found| found.len())
        },
    )
    .unwrap();
}

#[test]
fn a_three_way_any_of_cycle_terminates() {
    assert_finishes_within(
        std::time::Duration::from_secs(5),
        "a three-way anyOf cycle did not terminate",
        || async {
            let (schemas, url) = seeded(json!({
                "type": "object",
                "properties": { "node": { "$ref": "#/definitions/node" } },
                "definitions": {
                    "node": { "anyOf": [
                        { "$ref": "#/definitions/node" },
                        { "$ref": "#/definitions/node" },
                        { "$ref": "#/definitions/node" }
                    ] }
                }
            }))
            .await;

            let keys = "node".parse::<Keys>().unwrap();

            schemas
                .schemas_at_path(&url, &Value::Null, &keys)
                .await
                .map(|found| found.len())
        },
    )
    .unwrap();
}
```

Each closure returns `found.len()` rather than the schemas themselves, because `Keys` holds
`rowan` nodes and is not `Send`, and `assert_finishes_within` moves its result across a
thread boundary.

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls -- a_cycle_through_composed_all_of_members_terminates a_composed_all_of_cycle_terminates a_three_way_any_of_cycle_terminates`

Expected: all three fail after roughly five seconds each, with the message passed to
`assert_finishes_within` — that is the bound firing, which is the right failure.

A test that fails instantly with a different message means the fixture is wrong, not the
code. A test that runs for a minute and then reports `No such file or directory` means the
bound is not being enforced: the traversal is on the test's own thread, where nothing can
stop it, and the LRU has expired underneath it.

- [ ] **Step 3: Give `ref_schema_value` its URL back**

In `crates/taplo-common/src/schema/mod.rs`, change `ref_schema_value` to return the URL beside the value:

```rust
    /// The schema a `$ref` names, with the URL it resolved to.
    ///
    /// The URL is what the visited set is keyed on, so it has to travel with
    /// the value rather than be recomputed by the caller.
    async fn ref_schema_value(
        &self,
        root_url: &Url,
        schema: &Value,
    ) -> Option<(Url, Arc<Value>)> {
        let r = schema.schema_ref()?;

        let url = match reference_url(root_url, r) {
            Some(u) => u,
            None => {
                tracing::error!(reference = r, "could not determine schema URL");
                return None;
            }
        };

        match self.resolve_schema(url.clone()).await {
            Ok(s) => Some((url, s)),
            Err(error) => {
                tracing::error!(?error, "failed to resolve schema");
                None
            }
        }
    }
```

`condition_holds` is its other caller and destructures the old return type, so it stops
compiling here. Change its one line:

```rust
        let condition = resolved.as_ref().map_or(condition, |(_, schema)| &**schema);
```

replacing `let condition = resolved.as_deref().unwrap_or(condition);`. Without this the
build fails at that line with `E0599: as_deref exists for Option<(Url, Arc<Value>)> but its
trait bounds were not satisfied`.

- [ ] **Step 4: Add the visited set to both traversals**

Add the parameter to `collect_schemas`, immediately before `schemas`:

```rust
        composition_depth: usize,
        visited: &mut Vec<Url>,
        schemas: &mut Vec<(Keys, Arc<Value>)>,
```

Replace its `$ref` arm with one that pushes, recurses and pops:

```rust
        if let Some(r) = schema.schema_ref() {
            let url = reference_url(root_url, r)
                .ok_or_else(|| anyhow!("could not determine schema URL"))?;

            // A reference already followed on this chain leads back to a schema
            // whose contribution is already in the accumulator, so following it
            // again only multiplies the work a cycle costs.
            if visited.contains(&url) {
                return Ok(false);
            }

            let schema = self.resolve_schema(url.clone()).await?;

            visited.push(url);
            let evaluated = self
                .collect_schemas(
                    root_url,
                    &schema,
                    value,
                    full_path.clone(),
                    path,
                    composition_depth,
                    visited,
                    schemas,
                )
                .await;
            visited.pop();

            return evaluated;
        }
```

Every in-place recursion in `collect_schemas` — the `oneOf`, `anyOf` and `allOf` loops, and the `conditional_subschemas` loop — passes `visited` through unchanged. Every descent that consumes a path segment starts a fresh set, beside the `MAX_COMPOSITION_DEPTH` it already resets:

```rust
                        MAX_COMPOSITION_DEPTH,
                        &mut Vec::new(),
                        schemas,
```

Apply that to all six path-consuming calls in the `KeyOrIndex::Key` arm and the one in the `KeyOrIndex::Index` arm.

`schemas_at_path` passes a fresh set:

```rust
                MAX_COMPOSITION_DEPTH,
                &mut Vec::new(),
                &mut schemas,
```

Do the same to `collect_child_schemas`. Its `$ref` arm:

```rust
        if let Some((url, resolved)) = self.ref_schema_value(root_url, schema).await {
            if visited.contains(&url) {
                return;
            }

            visited.push(url);
            self.collect_child_schemas(
                root_url,
                &resolved,
                root_path,
                path,
                instance,
                depth,
                composition_depth,
                visited,
                schemas,
            )
            .await;
            visited.pop();

            return;
        }
```

Its composed-`allOf` branch both consults and feeds the set. A member already on the chain is left out of the merge, and a member that is merged is on the chain for the recursion that follows:

```rust
                let mut merged_all_of = Value::Object(serde_json::Map::default());
                let mut merged_urls = Vec::new();

                for all_of in all_ofs {
                    match self.ref_schema_value(root_url, all_of).await {
                        Some((url, resolved)) => {
                            if visited.contains(&url) || merged_urls.contains(&url) {
                                continue;
                            }
                            merged_urls.push(url);
                            merged_all_of.merge(&resolved);
                        }
                        None => merged_all_of.merge(all_of),
                    }
                }

                merged_all_of.merge(&schema);

                let merged_count = merged_urls.len();
                visited.append(&mut merged_urls);

                self.collect_child_schemas(
                    root_url,
                    &merged_all_of,
                    root_path,
                    path,
                    instance,
                    depth,
                    composition_depth,
                    visited,
                    schemas,
                )
                .await;

                visited.truncate(visited.len() - merged_count);
```

This arm changes twice more: the tuple gains the target's base in Task 3, and Task 5 makes
each member's own references absolute before merging it. Neither belongs here.

Its `properties` loop starts a fresh set, beside the `MAX_COMPOSITION_DEPTH` it already resets. `possible_schemas_from` passes a fresh set per schema.

- [ ] **Step 5: Run the tests and watch them pass**

Run: `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls`

Expected: PASS. If `mutually_referential_all_of_terminates` or `self_referential_all_of_terminates` now returns a *different* set of schemas rather than merely faster, stop: the set is suppressing a reference it should follow, which means a `pop` is missing.

- [ ] **Step 6: Run the whole workspace and the wasm check**

Run:
```
cargo check --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml --workspace --all-targets
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml --workspace
cargo check --target wasm32-unknown-unknown --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-wasm/Cargo.toml
```
Expected: clean, with the two inherited `dead_code` warnings.

- [ ] **Step 7: Format and commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/mod.rs /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution add crates/taplo-common/src/schema/mod.rs crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution commit -m "$(cat <<'EOF'
perf(schema): stop revisiting a ref in one chain

A composition cycle spent one unit of the depth budget per hop, so it ran
sixteen rounds and a branching cycle ran its fan-out to that power. A
three-way anyOf cycle took 63 seconds; a composed allOf of three did not
finish. Both traversals now carry the set of reference URLs resolved since
the last path-consuming descent and refuse one already on it.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Resolve a reference as a URI reference

`reference_url` recognizes a `#…` fragment and a fully absolute URL and rejects everything else, so `common.json#/definitions/port` yields `None` and `collect_schemas` returns `Err("could not determine schema URL")` — which every handler renders as nothing at all. It also strips the leading `/` from a same-document fragment while `Url::parse` keeps it, and `resolve_schema` prepends one to both, so `https://example.com/x.json#/definitions/port` becomes the pointer `//definitions/port` and finds nothing.

**Files:**
- Modify: `crates/taplo-common/src/schema/mod.rs` (`reference_url`, `resolve_schema`)
- Test: `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Consumes: `ref_schema_value(&self, root_url, schema) -> Option<(Url, Arc<Value>)>` from Task 1.
- Produces: `fn reference_url(base: &Url, reference: &str) -> Option<Url>` — now one `Url::join`.
- Produces: `async fn seeded_documents(documents: &[(&str, Value)]) -> (Schemas<NativeEnvironment>, Url)` in `tests.rs` — seeds several documents, the first of which is the root, and returns its URL.

- [ ] **Step 1: Write the failing tests**

Append to `crates/taplo-common/src/schema/tests.rs`:

```rust
/// Seeds several documents into the in-memory cache and returns the URL of the
/// first, which is the root. Paths are relative to `file:///taplo-test/`.
async fn seeded_documents(documents: &[(&str, Value)]) -> (Schemas<NativeEnvironment>, Url) {
    let schemas = Schemas::new(NativeEnvironment::new(), reqwest::Client::new());
    let base = Url::parse("file:///taplo-test/").unwrap();

    let mut root = None;

    for (path, document) in documents {
        let url = base.join(path).unwrap();
        drop(
            schemas
                .cache()
                .store(url.clone(), Arc::new(document.clone()))
                .await,
        );
        root.get_or_insert(url);
    }

    (schemas, root.expect("no documents seeded"))
}

#[tokio::test]
async fn a_relative_reference_resolves_against_the_document() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": { "port": { "$ref": "common.json#/definitions/port" } }
            }),
        ),
        (
            "common.json",
            json!({
                "definitions": { "port": { "description": "relative", "type": "integer" } }
            }),
        ),
    ])
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["relative"]);
}

#[tokio::test]
async fn a_reference_walks_out_of_the_document_directory() {
    let (schemas, url) = seeded_documents(&[
        (
            "nested/schema.json",
            json!({
                "type": "object",
                "properties": {
                    "up": { "$ref": "../common.json#/definitions/port" },
                    "rooted": { "$ref": "/taplo-test/common.json#/definitions/port" }
                }
            }),
        ),
        (
            "common.json",
            json!({
                "definitions": { "port": { "description": "shared", "type": "integer" } }
            }),
        ),
    ])
    .await;

    for key in ["up", "rooted"] {
        let found = schemas
            .schemas_at_path(&url, &Value::Null, &key.parse::<Keys>().unwrap())
            .await
            .unwrap();
        assert_eq!(descriptions(&found), ["shared"], "at `{key}`");
    }
}

#[tokio::test]
async fn an_absolute_reference_carrying_a_pointer_resolves() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": {
                    "port": { "$ref": "file:///taplo-test/common.json#/definitions/port" }
                }
            }),
        ),
        (
            "common.json",
            json!({
                "definitions": { "port": { "description": "absolute", "type": "integer" } }
            }),
        ),
    ])
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["absolute"]);
}

#[tokio::test]
async fn a_reference_to_the_whole_document_resolves() {
    let (schemas, url) = seeded(json!({
        "description": "the root",
        "type": "object",
        "properties": { "child": { "$ref": "#" } }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"child".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["the root"]);
}

#[tokio::test]
async fn a_fragment_is_percent_decoded_and_unescaped() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "spaced": { "$ref": "#/definitions/two words" },
            "slashed": { "$ref": "#/definitions/a~1b" }
        },
        "definitions": {
            "two words": { "description": "spaced", "type": "integer" },
            "a/b": { "description": "slashed", "type": "integer" }
        }
    }))
    .await;

    for (key, expected) in [("spaced", "spaced"), ("slashed", "slashed")] {
        let found = schemas
            .schemas_at_path(&url, &Value::Null, &key.parse::<Keys>().unwrap())
            .await
            .unwrap();
        assert_eq!(descriptions(&found), [expected], "at `{key}`");
    }
}

#[test]
fn a_relative_reference_under_an_https_base_joins_to_https() {
    let base = Url::parse("https://example.com/schemas/root.json").unwrap();

    assert_eq!(
        reference_url(&base, "common.json#/definitions/port")
            .unwrap()
            .as_str(),
        "https://example.com/schemas/common.json#/definitions/port"
    );
}

/// A document that references itself at every level, queried deep enough that
/// the branching would show if the visited set were not consulted. It belongs
/// here rather than with the other cycle tests, because `$ref: "#"` does not
/// resolve at all until this task lands.
#[test]
fn a_self_referential_document_terminates_at_depth() {
    assert_finishes_within(
        std::time::Duration::from_secs(5),
        "a self-referential document did not terminate at depth 8",
        || async {
            let (schemas, url) = seeded(json!({
                "anyOf": [{ "$ref": "#" }, { "$ref": "#" }],
                "properties": { "a": { "$ref": "#" } }
            }))
            .await;

            let keys = "a.a.a.a.a.a.a.a".parse::<Keys>().unwrap();

            schemas
                .schemas_at_path(&url, &Value::Null, &keys)
                .await
                .map(|found| found.len())
        },
    )
    .unwrap();
}

/// Traversal applies a fragment as a JSON pointer to the raw document, so the
/// name of the definition container is a key like any other and no draft is
/// consulted. Pinned so a later change to fragment handling cannot break it
/// quietly.
#[tokio::test]
async fn defs_and_definitions_resolve_under_every_draft() {
    for meta in [
        "http://json-schema.org/draft-07/schema#",
        "https://json-schema.org/draft/2019-09/schema",
        "https://json-schema.org/draft/2020-12/schema",
    ] {
        let (schemas, url) = seeded(json!({
            "$schema": meta,
            "type": "object",
            "properties": {
                "new": { "$ref": "#/$defs/port" },
                "old": { "$ref": "#/definitions/port" }
            },
            "$defs": { "port": { "description": "defs", "type": "integer" } },
            "definitions": { "port": { "description": "definitions", "type": "integer" } }
        }))
        .await;

        for (key, expected) in [("new", "defs"), ("old", "definitions")] {
            let found = schemas
                .schemas_at_path(&url, &Value::Null, &key.parse::<Keys>().unwrap())
                .await
                .unwrap();
            assert_eq!(descriptions(&found), [expected], "at `{key}` under {meta}");
        }
    }
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls`

Expected: `defs_and_definitions_resolve_under_every_draft` PASSES — `$defs` already works, and this test exists to keep it working. The other five fail:

- `a_relative_reference_resolves_against_the_document`, `a_reference_walks_out_of_the_document_directory` — `called Result::unwrap() on an Err value: could not determine schema URL`.
- `an_absolute_reference_carrying_a_pointer_resolves`, `a_reference_to_the_whole_document_resolves` — `failed to resolve relative schema`.
- `a_fragment_is_percent_decoded_and_unescaped` — the `spaced` case fails to resolve; the `slashed` case may pass, since `~1` survives today's path.
- `a_relative_reference_under_an_https_base_joins_to_https` — `called Option::unwrap() on a None value`.
- `a_self_referential_document_terminates_at_depth` — the traversal errors on `$ref: "#"`, so the closure returns `Err` and `.unwrap()` panics. This is the one test here that fails for a reason other than the assertion; after Step 4 it terminates because the visited set stops it.

- [ ] **Step 3: Replace `reference_url` with a join**

In `crates/taplo-common/src/schema/mod.rs`:

```rust
/// The absolute URL a `$ref` denotes, resolved against the base in force where
/// it is written.
///
/// This is RFC 3986 reference resolution, which `Url::join` implements: a
/// fragment-only reference names this document, a relative path names a
/// sibling, an absolute URL names itself. A join fails only for a base that
/// cannot be one, which no scheme reaching `fetch_external` produces.
fn reference_url(base: &Url, reference: &str) -> Option<Url> {
    base.join(reference).ok()
}
```

- [ ] **Step 4: Read the fragment as written**

Replace `resolve_schema`'s fragment arm. The fragment now arrives as the reference wrote it, leading `/` intact and percent-encoded, so the pointer is the fragment decoded rather than a `/` prepended to it:

```rust
    #[async_recursion(?Send)]
    #[must_use]
    pub(crate) async fn resolve_schema(&self, url: Url) -> Result<Arc<Value>, anyhow::Error> {
        let fragment = url.fragment().unwrap_or_default();

        // An absent or empty fragment names the whole document; a fragment
        // starting with `/` is a JSON pointer. A URI fragment is
        // percent-encoded and a JSON pointer is not, so it is decoded before
        // use; `~0` and `~1` survive that untouched and `serde_json` unescapes
        // them itself.
        if fragment.is_empty() {
            let mut document_url = url.clone();
            document_url.set_fragment(None);
            let val = self.load_schema(&document_url).await?;
            drop(self.cache.store(document_url, val.clone()));
            return Ok(val);
        }

        let mut document_url = url.clone();
        document_url.set_fragment(None);
        let document = self.resolve_schema(document_url).await?;

        let pointer = percent_encoding::percent_decode_str(fragment)
            .decode_utf8()
            .with_context(|| format!("reference fragment is not valid UTF-8: {fragment}"))?;

        if !pointer.starts_with('/') {
            return Err(anyhow!("could not resolve reference fragment `{pointer}`"));
        }

        document
            .pointer(&pointer)
            .map(|v| Arc::new(v.clone()))
            .ok_or_else(|| anyhow!("failed to resolve reference `{url}`"))
    }
```

The recursion is now on the fragment-less URL only, so it is one level deep rather than the mutual recursion the old shape had.

- [ ] **Step 5: Run the tests and watch them pass**

Run: `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls`

Expected: PASS. If `a_self_referential_document_terminates_at_depth` times out rather than
passing, the visited set from Task 1 is not being consulted on the `#` shape.

- [ ] **Step 6: Run the whole workspace and the wasm check**

Run the three commands from Task 1 Step 6. Expected: clean.

- [ ] **Step 7: Format and commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/mod.rs /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution add crates/taplo-common/src/schema/mod.rs crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution commit -m "$(cat <<'EOF'
fix(schema): resolve a ref as a uri reference

Reference resolution recognized a fragment and an absolute URL and rejected
everything else, so a relative reference errored and took the whole request
with it. It also stripped the leading slash from a same-document fragment
and prepended one to every fragment, so an absolute URL carrying a pointer
resolved to a pointer nothing matches.

Url::join is that resolution, and the fragment it produces is read as
written: empty for the whole document, percent-decoded as a JSON pointer
otherwise.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Carry a base URL through traversal

`root_url` is fixed for the whole traversal, so a relative reference written inside a document reached through another reference resolves against the *root* document's directory rather than its own. And `$id`, which is the rule for how a base moves, is read nowhere. Both are the same mechanism, and validation already implements it: probed, a document carrying `{"$id": "defs/", "$ref": "port.json"}` resolves `port.json` against `defs/` in `jsonschema` and errors in traversal.

**Files:**
- Modify: `crates/taplo-common/src/schema/mod.rs` (`resolve_schema`, `collect_schemas`, `collect_child_schemas`, `ref_schema_value`, `condition_holds`, `conditional_subschemas`)
- Test: `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Consumes: `reference_url(base, reference)` from Task 2.
- Produces: `async fn resolve_schema(&self, url: Url) -> Result<(Url, Arc<Value>), anyhow::Error>` — the base in force *around* the target: the document's URL joined with every `$id` the pointer walked *through*, not the target's own, which the traversal applies on entry.
- Produces: `fn rebase(base: &Url, schema: &Value) -> Option<Url>` — the base a schema object's `$id` establishes, or `None` when it declares none.
- Produces: the first parameter of both traversals is renamed `base_url` and now varies.

- [ ] **Step 1: Write the failing tests**

Append to `crates/taplo-common/src/schema/tests.rs`:

```rust
#[tokio::test]
async fn an_inner_id_rebases_the_references_beneath_it() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": { "port": { "$ref": "#/definitions/wrapper" } },
                "definitions": {
                    "wrapper": { "$id": "defs/", "$ref": "port.json" }
                }
            }),
        ),
        (
            "defs/port.json",
            json!({ "description": "rescoped", "type": "integer" }),
        ),
    ])
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["rescoped"]);
}

#[tokio::test]
async fn a_reference_rebases_at_the_document_it_reaches() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": { "port": { "$ref": "sub/inner.json#/definitions/port" } }
            }),
        ),
        (
            "sub/inner.json",
            json!({ "definitions": { "port": { "$ref": "port.json" } } }),
        ),
        (
            "sub/port.json",
            json!({ "description": "beside inner", "type": "integer" }),
        ),
    ])
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["beside inner"]);
}

/// A pointer collects an `$id` from every object it walks through, which is
/// what `jsonschema`'s own `resolve_fragment` does through `join_folders`.
#[tokio::test]
async fn a_pointer_crossing_an_id_rebases_beneath_it() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": { "port": { "$ref": "#/definitions/sub/properties/x" } },
                "definitions": {
                    "sub": {
                        "$id": "sub/",
                        "properties": { "x": { "$ref": "port.json" } }
                    }
                }
            }),
        ),
        (
            "sub/port.json",
            json!({ "description": "under sub", "type": "integer" }),
        ),
    ])
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["under sub"]);
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls`

Expected: the three new tests fail. `an_inner_id_rebases_the_references_beneath_it` and `a_pointer_crossing_an_id_rebases_beneath_it` fail with `No such file or directory` — the reference resolved to `file:///taplo-test/port.json`, which is not seeded, so `load_schema` tries the filesystem. `a_reference_rebases_at_the_document_it_reaches` fails the same way. A `could not determine schema URL` here means Task 2 did not land.

- [ ] **Step 3: Add the rebasing rule**

Add beside `instance_at` in `crates/taplo-common/src/schema/mod.rs`:

```rust
/// The base a schema object establishes for everything written inside it.
///
/// `$id` is a URI reference like any other, resolved against the base in force
/// where it appears; its fragment names the object rather than a document, so
/// it is dropped from the base. Draft 4 spells this keyword `id`, which
/// traversal does not read: `id` stopped being a keyword in draft 6, and a
/// schema still carrying one as leftover metadata would otherwise re-base
/// every reference beneath it.
fn rebase(base: &Url, schema: &Value) -> Option<Url> {
    let id = schema["$id"].as_str()?;
    let mut url = base.join(id).ok()?;
    url.set_fragment(None);
    Some(url)
}
```

- [ ] **Step 4: Return the base from `resolve_schema`**

Collect an `$id` from every object the pointer walks through, and hand the caller the base it arrived at:

```rust
    /// The schema at `url`, with the base in force *around* it.
    ///
    /// A JSON pointer may cross objects that carry `$id`, each of which
    /// re-bases what lies beneath it, so the base is the document's URL joined
    /// with every `$id` the pointer walked *through*. The target's own `$id` is
    /// deliberately not applied: every consumer re-bases on entry, and applying
    /// it here as well would join it twice — `{"$id": "defs/"}` would resolve a
    /// sibling reference against `defs/defs/`.
    #[must_use]
    pub(crate) async fn resolve_schema(
        &self,
        url: Url,
    ) -> Result<(Url, Arc<Value>), anyhow::Error> {
        let mut document_url = url.clone();
        document_url.set_fragment(None);

        let document = self.load_schema(&document_url).await?;
        drop(self.cache.store(document_url.clone(), document.clone()));

        let fragment = url.fragment().unwrap_or_default();
        if fragment.is_empty() {
            return Ok((document_url, document));
        }

        let pointer = percent_encoding::percent_decode_str(fragment)
            .decode_utf8()
            .with_context(|| format!("reference fragment is not valid UTF-8: {fragment}"))?;

        if !pointer.starts_with('/') {
            return Err(anyhow!("could not resolve reference fragment `{pointer}`"));
        }

        let mut base = document_url;
        let mut target = &*document;

        for token in pointer.split('/').skip(1) {
            // The `$id` of an object the pointer passes *through* re-bases what
            // lies below it. The one on the object the pointer lands on is left
            // to the traversal, which re-bases on entry.
            if let Some(rebased) = rebase(&base, target) {
                base = rebased;
            }

            let token = token.replace("~1", "/").replace("~0", "~");

            target = match target {
                Value::Object(map) => map.get(&token),
                Value::Array(items) => token.parse::<usize>().ok().and_then(|i| items.get(i)),
                _ => None,
            }
            .ok_or_else(|| anyhow!("failed to resolve reference `{url}`"))?;
        }

        Ok((base, Arc::new(target.clone())))
    }
```

`async_recursion` comes off: the function no longer calls itself.

- [ ] **Step 5: Thread the base through both traversals**

Rename the first parameter of `collect_schemas` and `collect_child_schemas` from `root_url` to `base_url`, and of `ref_schema_value` and `condition_holds` and `conditional_subschemas` likewise. In each traversal, re-base on entry, immediately after the guard:

```rust
        let rebased = rebase(base_url, schema);
        let base_url = rebased.as_ref().unwrap_or(base_url);
```

At the `$ref` arm of `collect_schemas`, the resolved base replaces the current one for the target:

```rust
            let (target_base, schema) = self.resolve_schema(url.clone()).await?;

            visited.push(url);
            let evaluated = self
                .collect_schemas(
                    &target_base,
                    &schema,
                    value,
                    full_path.clone(),
                    path,
                    composition_depth,
                    visited,
                    schemas,
                )
                .await;
            visited.pop();

            return evaluated;
```

`ref_schema_value` returns the target's base too:

```rust
    async fn ref_schema_value(
        &self,
        base_url: &Url,
        schema: &Value,
    ) -> Option<(Url, Url, Arc<Value>)> {
```

returning `(reference_url, target_base, value)`. `condition_holds` destructures it and has to
follow, or the build fails at that line:

```rust
        let condition = resolved.as_ref().map_or(condition, |(_, _, schema)| &**schema);
``` Its two callers in `collect_child_schemas` — the `$ref` arm and the composed-`allOf` merge — take the base from it; the merge keeps `base_url` for the merged value, because a merged object is not any one member's document.

Every other recursion passes `base_url` unchanged, including the path-consuming ones: a property descent stays in the document it is written in.

- [ ] **Step 6: Run the tests and watch them pass**

Run: `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls`

Expected: PASS.

- [ ] **Step 7: Run the whole workspace and the wasm check**

Run the three commands from Task 1 Step 6. Expected: clean.

- [ ] **Step 8: Format and commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/mod.rs /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution add crates/taplo-common/src/schema/mod.rs crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution commit -m "$(cat <<'EOF'
feat(schema): carry a base url through traversal

Traversal resolved every reference against the root document's URL, so a
relative reference written in a document reached through another reference
resolved against the wrong directory, and $id was read nowhere.

The base now travels with the schema: it moves at an object carrying $id,
and at each reference it becomes the target's own, after every $id the
pointer crossed. That is the rule jsonschema's resolver already follows.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Resolve a plain-name anchor

A `$ref` whose fragment is a plain name — `#port` — names the subschema whose canonical URI ends in that fragment. `jsonschema` answers it from an index `find_schemas` builds at compile time; probed, `{"$ref": "#port"}` against `{"$id": "#port"}` resolves in validation and errors in traversal. `$anchor` is *not* part of this: `jsonschema` 0.17.1 has no arm for it, and probed, the same reference against `{"$anchor": "port"}` validates as `InvalidReference`. Traversal matches the validator and resolves `$id` only.

**Files:**
- Modify: `crates/taplo-common/src/schema/mod.rs` (`resolve_schema`, new `anchored_subschema`)
- Test: `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Consumes: `rebase(base, schema)` and `resolve_schema(url) -> Result<(Url, Arc<Value>)>` from Task 3.
- Produces: `fn anchored_subschema<'d>(document: &'d Value, base: &Url, url: &Url) -> Option<(Url, &'d Value)>`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/taplo-common/src/schema/tests.rs`:

```rust
#[tokio::test]
async fn a_plain_name_fragment_resolves_to_the_id_that_claims_it() {
    let (schemas, url) = seeded(json!({
        "$id": "file:///taplo-test/schema.json",
        "type": "object",
        "properties": { "port": { "$ref": "#port" } },
        "definitions": {
            "port": { "$id": "#port", "description": "anchored", "type": "integer" }
        }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["anchored"]);
}

/// `jsonschema` 0.17.1 indexes `$id` and nothing else, so `$anchor` resolves in
/// neither half. Asserted of both, so the divergence stays visible rather than
/// becoming a silent difference between what validates and what completes.
#[tokio::test]
async fn an_anchor_keyword_resolves_in_neither_half() {
    let (schemas, url) = seeded(json!({
        "$id": "file:///taplo-test/schema.json",
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": { "port": { "$ref": "#port" } },
        "$defs": {
            "port": { "$anchor": "port", "description": "anchored", "type": "integer" }
        }
    }))
    .await;

    assert!(schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .is_err());

    let errors = schemas
        .validate(&url, &json!({ "port": "not an integer" }))
        .await
        .unwrap();

    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            jsonschema::error::ValidationErrorKind::InvalidReference { .. }
        )),
        "expected an invalid-reference error, got {errors:?}"
    );
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls -- a_plain_name_fragment_resolves_to_the_id_that_claims_it an_anchor_keyword_resolves_in_neither_half`

Expected: `a_plain_name_fragment_resolves_to_the_id_that_claims_it` fails with `could not resolve reference fragment `port``. `an_anchor_keyword_resolves_in_neither_half` PASSES already, and exists to keep the two halves aligned.

- [ ] **Step 3: Write the anchor walk**

Add beside `rebase` in `crates/taplo-common/src/schema/mod.rs`:

```rust
/// The subschema in `document` whose `$id` resolves to `url`, with the base it
/// establishes.
///
/// Mirrors the index `jsonschema` builds at compile time: `$id` joins onto the
/// base in force and re-bases everything beneath it, so a subschema's canonical
/// URI is the chain of `$id`s above it. `enum` and `const` are skipped, because
/// their contents are instance data and a key named `$id` inside one is a
/// value, not an identifier.
fn anchored_subschema<'d>(
    document: &'d Value,
    base: &Url,
    url: &Url,
) -> Option<(Url, &'d Value)> {
    let declared = document["$id"].as_str().and_then(|id| base.join(id).ok());

    // The base returned is the one in force *around* the match, not the one its
    // own `$id` establishes: the traversal re-bases on entry, and applying it
    // here as well would join it twice.
    if declared.as_ref() == Some(url) {
        return Some((base.clone(), document));
    }

    let base = declared.map_or_else(
        || base.clone(),
        |mut d| {
            d.set_fragment(None);
            d
        },
    );

    match document {
        Value::Object(map) => map
            .iter()
            .filter(|(k, _)| *k != "enum" && *k != "const")
            .find_map(|(_, v)| anchored_subschema(v, &base, url)),
        Value::Array(items) => items
            .iter()
            .find_map(|item| anchored_subschema(item, &base, url)),
        _ => None,
    }
}
```

- [ ] **Step 4: Use it for a fragment that is not a pointer**

In `resolve_schema`, replace the error arm for a non-pointer fragment:

```rust
        if !pointer.starts_with('/') {
            return anchored_subschema(&document, &document_url, &url)
                .map(|(anchor_base, schema)| (anchor_base, Arc::new(schema.clone())))
                .ok_or_else(|| anyhow!("failed to resolve reference `{url}`"));
        }
```

`url` here is the reference's full URL, fragment included, which is the canonical URI an
`$id` has to match. The base handed in is the *document's* URL rather than a pre-rebased one,
because `anchored_subschema` applies the root `$id` itself on its first step.

- [ ] **Step 5: Run the tests and watch them pass**

Run: `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls`

Expected: PASS.

- [ ] **Step 6: Run the whole workspace and the wasm check**

Run the three commands from Task 1 Step 6. Expected: clean.

- [ ] **Step 7: Format and commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/mod.rs /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution add crates/taplo-common/src/schema/mod.rs crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution commit -m "$(cat <<'EOF'
feat(schema): resolve a plain-name anchor

A fragment that is not a JSON pointer names the subschema whose $id claims
it. Validation resolves one through the index jsonschema builds at compile
time; traversal errored, which cost every path beneath such a reference its
hover and its completion.

$anchor is deliberately not read: jsonschema 0.17.1 indexes $id alone, so
resolving it would make traversal claim what validation rejects.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Apply the keywords written beside a reference

Both traversals return at `$ref` without reading the object that carries it, so `{"$ref": "#/definitions/port", "description": "…"}` shows the target's description and not the author's, and `{"$ref": "…", "unevaluatedProperties": {…}}` loses the keyword — the one place the applicator feature set's coverage predicate diverges from the specification. The target is merged *under* the carrier's keys, so one schema comes out rather than two; yielding both is what doubles value completion.

**Files:**
- Modify: `crates/taplo-common/src/schema/mod.rs` (`collect_schemas`, `collect_child_schemas`, new `merged_over`, `absolute_refs`, `sibling_overlay`)
- Modify: `crates/taplo-lsp/src/handlers/hover.rs` (`mod tests` only: `world_with`, `world_with_documents`, `complete_at_line`, `complete_at_documents`)
- Test: `crates/taplo-common/src/schema/tests.rs`, `crates/taplo-lsp/src/handlers/hover.rs`, `crates/taplo-lsp/src/handlers/completion.rs`

**Interfaces:**
- Consumes: `reference_url(base, reference)`, `rebase(base, schema)`, `ref_schema_value(base_url, schema) -> Option<(Url, Url, Arc<Value>)>`.
- Produces: `fn sibling_overlay(schema: &Value) -> Option<Value>` — the carrier minus the keywords that identify it, or `None` when nothing applicable is left.
- Produces: `fn merged_over(base: &Value, overlay: &Value) -> Value`.
- Produces: `fn absolute_refs(schema: &Value, base: &Url) -> Value`.
- Produces: `fn fold_siblings(&self, target: Arc<Value>, carrier: &Value, base_url: &Url) -> Arc<Value>` — the target with the carrier's applicable siblings merged over it, or the target unchanged when there are none.
- Produces: `async fn world_with_documents(schemas: &[(&str, serde_json::Value)], source: &str) -> (Arc<WorldState<NativeEnvironment>>, Url)` in `hover.rs`'s `mod tests`.

- [ ] **Step 1: Write the failing traversal tests**

Append to `crates/taplo-common/src/schema/tests.rs`:

```rust
#[tokio::test]
async fn a_sibling_description_wins_over_the_target() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "port": { "$ref": "#/definitions/port", "description": "the sibling" }
        },
        "definitions": {
            "port": { "description": "the target", "type": "integer" }
        }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["the sibling"]);
    assert_eq!(found[0].1["type"], "integer");
    assert_eq!(found.len(), 1, "the carrier and the target are one schema");
}

#[tokio::test]
async fn a_sibling_enum_replaces_the_target_enum() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": { "kind": { "$ref": "#/definitions/kind", "enum": ["a"] } },
        "definitions": { "kind": { "type": "string", "enum": ["b", "c"] } }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"kind".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(found[0].1["enum"], json!(["a"]));
}

#[tokio::test]
async fn a_sibling_required_unions_with_the_target() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "server": { "$ref": "#/definitions/server", "required": ["extra"] }
        },
        "definitions": { "server": { "type": "object", "required": ["name"] } }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"server".parse::<Keys>().unwrap())
        .await
        .unwrap();

    let required = found[0].1["required"].as_array().unwrap();
    assert!(required.contains(&json!("name")) && required.contains(&json!("extra")));
}

#[tokio::test]
async fn sibling_properties_union_with_the_target() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": {
                    "server": {
                        "$ref": "sub/server.json",
                        "properties": { "extra": { "$ref": "#/definitions/extra" } }
                    }
                },
                "definitions": { "extra": { "description": "sibling extra", "type": "string" } }
            }),
        ),
        (
            "sub/server.json",
            json!({
                "type": "object",
                "properties": { "name": { "description": "target name", "type": "string" } }
            }),
        ),
    ])
    .await;

    for (path, expected) in [("server.extra", "sibling extra"), ("server.name", "target name")] {
        let found = schemas
            .schemas_at_path(&url, &Value::Null, &path.parse::<Keys>().unwrap())
            .await
            .unwrap();
        assert_eq!(descriptions(&found), [expected], "at `{path}`");
    }
}

#[tokio::test]
async fn a_sibling_unevaluated_properties_applies() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "properties": {
            "obj": {
                "$ref": "#/definitions/obj",
                "unevaluatedProperties": { "description": "anything else", "type": "string" }
            }
        },
        "definitions": {
            "obj": { "type": "object", "properties": { "known": { "type": "string" } } }
        }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"obj.unknown".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["anything else"]);
}

/// A composed-`allOf` member is merged rather than recursed into, so it never
/// reaches the entry that re-bases. Its own pointers have to be made absolute
/// against its own document before the merge, or they resolve against the
/// carrier's.
#[tokio::test]
async fn a_composed_all_of_member_keeps_its_own_document() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "type": "object",
                "properties": {
                    "server": {
                        "description": "carrier",
                        "allOf": [{ "$ref": "sub/server.json" }]
                    }
                }
            }),
        ),
        (
            "sub/server.json",
            json!({
                "type": "object",
                "properties": { "name": { "$ref": "#/definitions/name" } },
                "definitions": {
                    "name": { "description": "member name", "type": "string" }
                }
            }),
        ),
    ])
    .await;

    let children = schemas
        .possible_schemas_from(&url, &Value::Null, &Keys::empty(), 5)
        .await
        .unwrap();

    let name = schema_at(&children, "server.name").expect("no schema for `server.name`");
    assert_eq!(name["description"], "member name", "got {name}");
}

/// `$defs` beside a `$ref` is not an applicable sibling: it is where the
/// reference points, not a keyword describing the instance. The root shape
/// `pydantic` emits stays on the fast path.
#[tokio::test]
async fn a_defs_container_beside_a_ref_is_not_a_sibling() {
    let (schemas, url) = seeded(json!({
        "$ref": "#/$defs/model",
        "$defs": {
            "model": {
                "type": "object",
                "properties": { "port": { "description": "the model", "type": "integer" } }
            }
        }
    }))
    .await;

    let found = schemas
        .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
        .await
        .unwrap();

    assert_eq!(descriptions(&found), ["the model"]);
}
```

- [ ] **Step 2: Write the failing handler tests**

Add `world_with_documents` to `crates/taplo-lsp/src/handlers/hover.rs`'s `mod tests`, and rewrite `world_with` to call it:

```rust
    /// Builds a world holding one document and several schemas, the first of
    /// which is associated with the document. Schema paths are relative to
    /// `file:///taplo-test/`.
    pub(crate) async fn world_with_documents(
        schemas: &[(&str, serde_json::Value)],
        source: &str,
    ) -> (Arc<WorldState<NativeEnvironment>>, Url) {
        let world = Arc::new(WorldState::new(NativeEnvironment::new()));
        let document_url: Url = "root:///test.toml".parse().unwrap();
        let base: Url = "file:///taplo-test/".parse().unwrap();

        {
            let mut workspaces = world.workspaces.write().await;
            let ws = workspaces.by_document_mut(&document_url);

            let mut root = None;

            for (path, schema) in schemas {
                let url = base.join(path).unwrap();
                drop(
                    ws.schemas
                        .cache()
                        .store(url.clone(), Arc::new(schema.clone()))
                        .await,
                );
                root.get_or_insert(url);
            }

            ws.schemas.associations().add(
                AssociationRule::glob("**/*.toml").unwrap(),
                SchemaAssociation {
                    url: root.expect("no schemas seeded"),
                    meta: json!({ "source": source::MANUAL }),
                    priority: priority::MAX,
                },
            );

            let parse = taplo::parser::parse(source);
            let mapper = Mapper::new_utf16(source, false);
            let dom = parse.clone().into_dom();

            ws.documents.insert(
                document_url.clone(),
                DocumentState { parse, mapper, dom },
            );
        }

        (world, document_url)
    }
```

`world_with_documents` takes over the whole body of the current `world_with`, including its
`DocumentState` construction and its doc comment about `Cache::store` and
`NativeEnvironment::new` — move them rather than retyping them, after reading the current
function with `rg -n -A 40 "async fn world_with" crates/taplo-lsp/src/handlers/hover.rs`.
`world_with` then becomes a one-line call, and since `base.join("schema.json")` produces the
`file:///taplo-test/schema.json` it seeded before, every existing test is unaffected:

```rust
    pub(crate) async fn world_with(
        schema: serde_json::Value,
        source: &str,
    ) -> (Arc<WorldState<NativeEnvironment>>, Url) {
        world_with_documents(&[("schema.json", schema)], source).await
    }
```

Then append a hover test to `crates/taplo-lsp/src/handlers/hover.rs`'s `mod tests`:

```rust
    #[tokio::test]
    async fn renders_a_description_written_beside_a_ref() {
        let hovered = hover_at(
            json!({
                "type": "object",
                "properties": {
                    "port": { "$ref": "#/definitions/port", "description": "The port to bind." }
                },
                "definitions": { "port": { "description": "An integer.", "type": "integer" } }
            }),
            "port = 8080",
            1,
        )
        .await
        .expect("no hover");

        assert!(
            hovered.contains("The port to bind."),
            "expected the sibling description, got {hovered}"
        );
    }
```

Give the completion helpers the same multi-document variant. The body of the current
`complete_at_line`, everything after its `world_with` call, moves into a private
`complete_in`; the two public helpers then differ only in how they build the world:

```rust
    pub(crate) async fn complete_at_line(
        schema: serde_json::Value,
        source: &str,
        line: u32,
        character: u32,
    ) -> Vec<lsp_types::CompletionItem> {
        let (world, document_url) = world_with(schema, source).await;
        complete_in(world, document_url, line, character).await
    }

    /// Returns the completion items the handler produces at a position, for a
    /// document whose schema is spread across several files.
    pub(crate) async fn complete_at_documents(
        schemas: &[(&str, serde_json::Value)],
        source: &str,
        line: u32,
        character: u32,
    ) -> Vec<lsp_types::CompletionItem> {
        let (world, document_url) = world_with_documents(schemas, source).await;
        complete_in(world, document_url, line, character).await
    }

    /// Runs the completion handler against a world that is already built.
    async fn complete_in(
        world: Arc<WorldState<NativeEnvironment>>,
        document_url: Url,
        line: u32,
        character: u32,
    ) -> Vec<lsp_types::CompletionItem> {
        // the body of the current `complete_at_line`, from its `completion(...)`
        // call to the end, unchanged
    }
```

`complete_in` stays private; `completion.rs`'s test module reaches `complete_at_documents`
through `super::super::hover::tests::`, because both the module and the function are
`pub(crate)`.

Then a completion test in `crates/taplo-lsp/src/handlers/completion.rs`'s `mod tests`:

```rust
    #[tokio::test]
    async fn offers_keys_of_a_target_reached_by_a_relative_reference() {
        let items = super::super::hover::tests::complete_at_documents(
            &[
                (
                    "schema.json",
                    json!({
                        "type": "object",
                        "properties": { "server": { "$ref": "sub/server.json" } }
                    }),
                ),
                (
                    "sub/server.json",
                    json!({
                        "type": "object",
                        "properties": {
                            "host": { "type": "string" },
                            "port": { "type": "integer" }
                        }
                    }),
                ),
            ],
            "[server]\n",
            1,
            0,
        )
        .await;

        let labels = labels(&items);
        assert!(
            labels.contains(&"host") && labels.contains(&"port"),
            "got {labels:?}"
        );
    }
```

- [ ] **Step 3: Run the tests and watch them fail**

Run:
```
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-lsp --lib handlers
```

Expected: `a_defs_container_beside_a_ref_is_not_a_sibling` and `offers_keys_of_a_target_reached_by_a_relative_reference` PASS — Tasks 2 and 3 made the second one work. The rest fail:

- `a_sibling_description_wins_over_the_target` reports `["the target"]`.
- `a_sibling_unevaluated_properties_applies` reports `[]`.
- `renders_a_description_written_beside_a_ref` finds `An integer.`
- `a_composed_all_of_member_keeps_its_own_document` finds the raw `{"$ref": "#/definitions/name"}` where it wants `"member name"`, because the member's pointer resolved against the carrier's document.

- [ ] **Step 4: Write the three helpers**

Add to `crates/taplo-common/src/schema/mod.rs`:

```rust
/// The keywords beside a `$ref` that describe the instance, or `None` when the
/// object carries only the reference and its own identity.
///
/// `$id`, `$anchor` and `$schema` identify the carrier rather than describe an
/// instance, and carrying one into the target would re-base the target's own
/// references onto the carrier's document. `definitions` and `$defs` hold what
/// the reference points *at*, so an object that carries only those — the root
/// shape `pydantic` and `schemars` emit — stays on the fast path.
fn sibling_overlay(schema: &Value) -> Option<Value> {
    const NOT_A_SIBLING: [&str; 7] = [
        "$ref",
        "$id",
        "$anchor",
        "$schema",
        "$comment",
        "definitions",
        "$defs",
    ];

    let map = schema.as_object()?;

    let overlay: serde_json::Map<_, _> = map
        .iter()
        .filter(|(k, _)| !NOT_A_SIBLING.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    (!overlay.is_empty()).then(|| Value::Object(overlay))
}

/// `overlay`'s keys win over `base`'s.
///
/// Where both hold an object the two merge recursively, which is what carries
/// `properties`, `patternProperties`, `dependentSchemas` and `x-taplo` — a
/// sibling `x-taplo.docs` lands beside the target's `x-taplo.links` rather than
/// erasing it. `const` and `default` are the exception: their objects are
/// instance data, and merging them would compose a value nobody wrote.
/// `required` is the union of both, because a validator applying both enforces
/// both and a sibling `required` was written to add an obligation, not to
/// cancel one. Every other array is replaced.
fn merged_over(base: &Value, overlay: &Value) -> Value {
    let (Some(base_map), Some(overlay_map)) = (base.as_object(), overlay.as_object()) else {
        return overlay.clone();
    };

    let mut merged = base_map.clone();

    for (key, value) in overlay_map {
        let merged_value = match (merged.get(key), key.as_str()) {
            (Some(Value::Array(existing)), "required") => {
                let mut union = existing.clone();
                for item in value.as_array().into_iter().flatten() {
                    if !union.contains(item) {
                        union.push(item.clone());
                    }
                }
                Value::Array(union)
            }
            (Some(existing), key) if key != "const" && key != "default" => {
                merged_over(existing, value)
            }
            _ => value.clone(),
        };

        merged.insert(key.clone(), merged_value);
    }

    Value::Object(merged)
}

/// A copy of `schema` whose every `$ref` string is absolute against `base`.
///
/// A subschema evaluated outside its document keeps its `#/...` pointers and
/// loses the document they name, so every reference fails to resolve. The walk
/// covers the subschema's own JSON and follows nothing, so it terminates
/// however cyclic the schema it came from is. A nested `$id` re-bases the
/// references beneath it, as it does in traversal. A reference that cannot be
/// joined is left as written.
fn absolute_refs(schema: &Value, base: &Url) -> Value {
    match schema {
        Value::Object(map) => {
            let base = rebase(base, schema).unwrap_or_else(|| base.clone());

            Value::Object(
                map.iter()
                    .map(|(key, value)| {
                        let value = match (key.as_str(), value.as_str()) {
                            ("$ref", Some(reference)) => reference_url(&base, reference)
                                .map_or_else(|| value.clone(), |u| Value::String(u.into())),
                            _ => absolute_refs(value, &base),
                        };
                        (key.clone(), value)
                    })
                    .collect(),
            )
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|i| absolute_refs(i, base)).collect())
        }
        other => other.clone(),
    }
}
```

- [ ] **Step 5: Merge at the reference in both traversals**

The fold lives in one place, so that the `$ref` arm of each traversal and the
composed-`allOf` merge all get it. Add it as a method beside `ref_schema_value`:

```rust
    /// `target` with the keywords written beside the `$ref` that named it
    /// merged over the top.
    ///
    /// The overlay's own references are written in the carrier's document, so
    /// they are made absolute against the carrier's base before the merge. The
    /// merged value is then traversed under the target's base, which is where
    /// everything the target itself wrote belongs.
    fn fold_siblings(
        &self,
        target: Arc<Value>,
        carrier: &Value,
        base_url: &Url,
    ) -> Arc<Value> {
        match sibling_overlay(carrier) {
            Some(overlay) => Arc::new(merged_over(&target, &absolute_refs(&overlay, base_url))),
            None => target,
        }
    }
```

`collect_schemas`'s `$ref` arm keeps resolving through `resolve_schema`, so that an
unresolvable reference stays the `Err` it is today, and folds the result:

```rust
            let (target_base, target) = self.resolve_schema(url.clone()).await?;
            let merged = self.fold_siblings(target, schema, base_url);

            visited.push(url);
            let evaluated = self
                .collect_schemas(
                    &target_base,
                    &merged,
                    value,
                    full_path.clone(),
                    path,
                    composition_depth,
                    visited,
                    schemas,
                )
                .await;
            visited.pop();

            return evaluated;
```

`&merged` is an `&Arc<Value>` where an `&Value` is wanted, which the deref coercion supplies
— the same shape the arm already had.

The composed-`allOf` merge in `collect_child_schemas` needs one more change, because a
member is merged rather than recursed into and so never reaches the entry that re-bases.
Its references are written in *its* document, and the merged value is traversed under the
carrier's, so they are made absolute before the merge:

```rust
                        Some((url, target_base, resolved)) => {
                            if visited.contains(&url) || merged_urls.contains(&url) {
                                continue;
                            }
                            merged_urls.push(url);
                            merged_all_of.merge(&absolute_refs(&resolved, &target_base));
                        }
```

`ref_schema_value` folds too, which gives the `$ref` arm of `collect_child_schemas` and its
composed-`allOf` merge the sibling behavior without a second copy of the rule:

```rust
    async fn ref_schema_value(
        &self,
        base_url: &Url,
        schema: &Value,
    ) -> Option<(Url, Url, Arc<Value>)> {
        let r = schema.schema_ref()?;

        let url = reference_url(base_url, r)?;

        let (target_base, target) = match self.resolve_schema(url.clone()).await {
            Ok(resolved) => resolved,
            Err(error) => {
                tracing::error!(?error, "failed to resolve schema");
                return None;
            }
        };

        Some((url, target_base, self.fold_siblings(target, schema, base_url)))
    }
```

- [ ] **Step 6: Run the tests and watch them pass**

Run:
```
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-lsp --lib handlers
```

Expected: PASS. The `taplo-lsp` handler suites gain two tests and lose none.

If `composed_all_of_ref_resolves_to_child_schema` fails, the overlay is being applied where the composed merge already applies one — check that `sibling_overlay` returns `None` for a bare `{"$ref": …}` member.

- [ ] **Step 7: Run the whole workspace and the wasm check**

Run the three commands from Task 1 Step 6. Expected: clean.

- [ ] **Step 8: Format and commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/mod.rs /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/tests.rs /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-lsp/src/handlers/hover.rs /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-lsp/src/handlers/completion.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution add crates/taplo-common/src/schema/mod.rs crates/taplo-common/src/schema/tests.rs crates/taplo-lsp/src/handlers/hover.rs crates/taplo-lsp/src/handlers/completion.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution commit -m "$(cat <<'EOF'
feat(schema): apply keywords written beside a ref

Traversal returned at a $ref without reading the object carrying it, so a
description written beside a reference never reached hover and a sibling
unevaluatedProperties never reached traversal at all.

The target is merged under the carrier's keys, so one schema comes out
rather than two; yielding both would double every value completion beneath
it. The carrier's own identity keywords and its definition containers are
not siblings, so the reference-only shape keeps the fast path.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Decide a condition that holds a reference

`condition_holds` refuses any condition containing a `$ref`, because a subschema compiled outside its document keeps its `#/...` pointers, loses the document they name, and rejects every instance — so an unguarded condition would pick `else` forever. `absolute_refs` removes the cause: probed, a condition whose reference is absolute against the document's URL evaluates `{"kind":"docker"}` as valid where the relative form evaluates it as invalid.

The guard that replaces `names_a_ref` is not a failed compile. `jsonschema` resolves a reference lazily, at evaluation, so a condition naming a document nothing has fetched *compiles cleanly* and `is_valid` reports plain `false`. `validate` distinguishes the two.

**Files:**
- Modify: `crates/taplo-common/src/schema/mod.rs` (`condition_holds`, remove `names_a_ref`)
- Test: `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Consumes: `absolute_refs(schema, base)` from Task 5, `load_schema(url)`.
- Produces: `condition_holds(&self, base_url, condition, instance) -> Option<bool>` — unchanged signature, new insides.

- [ ] **Step 1: Rewrite the inherited test and add the new ones**

Find `a_condition_holding_a_nested_reference_takes_both_branches` in `crates/taplo-common/src/schema/tests.rs` with `rg -n a_condition_holding_a_nested_reference /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/tests.rs`, read it, and replace it with:

```rust
#[tokio::test]
async fn a_condition_holding_a_nested_reference_decides_its_branch() {
    let schema = json!({
        "type": "object",
        "if": {
            "properties": { "kind": { "$ref": "#/definitions/isDocker" } },
            "required": ["kind"]
        },
        "then": { "properties": { "image": { "description": "then branch" } } },
        "else": { "properties": { "image": { "description": "else branch" } } },
        "definitions": { "isDocker": { "const": "docker" } }
    });

    for (kind, expected) in [("docker", "then branch"), ("podman", "else branch")] {
        let (schemas, url) = seeded(schema.clone()).await;

        let found = schemas
            .schemas_at_path(
                &url,
                &json!({ "kind": kind }),
                &"image".parse::<Keys>().unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(descriptions(&found), [expected], "for kind = {kind}");
    }
}

/// A reference the document cannot supply leaves the condition undecided, so
/// both branches are offered. Asserted through `schemas_at_path`, because
/// `is_valid` reports plain `false` for this and deciding by it would pick
/// `else` silently.
#[tokio::test]
async fn a_condition_holding_an_unresolvable_reference_takes_both_branches() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": {
            "properties": {
                "kind": { "$ref": "file:///taplo-test/missing.json#/definitions/isDocker" }
            },
            "required": ["kind"]
        },
        "then": { "properties": { "image": { "description": "then branch" } } },
        "else": { "properties": { "image": { "description": "else branch" } } }
    }))
    .await;

    let found = schemas
        .schemas_at_path(
            &url,
            &json!({ "kind": "docker" }),
            &"image".parse::<Keys>().unwrap(),
        )
        .await
        .unwrap();

    let mut found = descriptions(&found);
    found.sort_unstable();
    assert_eq!(found, ["else branch", "then branch"]);
}

#[tokio::test]
async fn a_condition_holding_a_dangling_pointer_takes_both_branches() {
    let (schemas, url) = seeded(json!({
        "type": "object",
        "if": {
            "properties": { "kind": { "$ref": "#/definitions/nope" } },
            "required": ["kind"]
        },
        "then": { "properties": { "image": { "description": "then branch" } } },
        "else": { "properties": { "image": { "description": "else branch" } } },
        "definitions": {}
    }))
    .await;

    let found = schemas
        .schemas_at_path(
            &url,
            &json!({ "kind": "docker" }),
            &"image".parse::<Keys>().unwrap(),
        )
        .await
        .unwrap();

    let mut found = descriptions(&found);
    found.sort_unstable();
    assert_eq!(found, ["else branch", "then branch"]);
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls -- a_condition_holding`

Expected: `a_condition_holding_a_nested_reference_decides_its_branch` fails, reporting both branches for both `kind` values — that is `names_a_ref` refusing to decide. The other two pass, since `names_a_ref` refuses those too; they are the regression net for the guard that replaces it.

- [ ] **Step 3: Delete `names_a_ref` and decide through `validate`**

Remove the `names_a_ref` function from `crates/taplo-common/src/schema/mod.rs` entirely, and rewrite `condition_holds`:

```rust
    /// Whether the instance satisfies a condition, or `None` when the condition
    /// cannot be decided and both branches have to be offered.
    ///
    /// A condition that is itself a reference is resolved, one hop, the way
    /// traversal resolves any schema carrying `$ref`. The references *inside*
    /// it are made absolute first: a subschema evaluated outside its document
    /// keeps its `#/...` pointers and loses the document they name, so every
    /// one of them would fail and the condition would reject every instance.
    ///
    /// The condition compiles through `create_validator`, which sees no
    /// `$schema` on a subschema and so compiles it as draft 7 whatever the root
    /// declares — the floor a draft-4 root needs, since draft 4 has no `const`
    /// to discriminate on.
    async fn condition_holds(
        &self,
        base_url: &Url,
        condition: &Value,
        instance: &Value,
    ) -> Option<bool> {
        if instance.is_null() {
            return None;
        }

        let resolved = self.ref_schema_value(base_url, condition).await;
        let (base_url, condition) = match &resolved {
            Some((_, base, schema)) => (base, &**schema),
            None => (base_url, condition),
        };

        let condition = absolute_refs(condition, base_url);

        let validator = match self.create_validator(&condition) {
            Ok(v) => v,
            Err(error) => {
                tracing::debug!(%error, "condition could not be compiled");
                return None;
            }
        };

        // A reference is resolved on evaluation rather than on compilation, so
        // a condition naming a document nothing has fetched compiles cleanly
        // and reports every instance invalid. `validate` names that case where
        // `is_valid` cannot, and an unfetched document is retrieved once and
        // the condition asked again, the way `validate_impl` does it.
        for attempt in 0..2 {
            let errors: Vec<_> = match validator.validate(instance) {
                Ok(()) => return Some(true),
                Err(errors) => errors.collect(),
            };

            let mut unresolved = None;

            for error in &errors {
                match &error.kind {
                    ValidationErrorKind::Resolver { url, .. } => unresolved = Some(url.clone()),
                    ValidationErrorKind::InvalidReference { .. } => return None,
                    _ => {}
                }
            }

            let Some(url) = unresolved else {
                return Some(false);
            };

            if attempt == 1 {
                return None;
            }

            match self.load_schema(&url).await {
                Ok(value) => drop(self.cache.store(url, value).await),
                Err(error) => {
                    tracing::debug!(%error, "condition names a schema that could not be loaded");
                    return None;
                }
            }
        }

        None
    }
```

`ValidationErrorKind` is already imported.

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls`

Expected: PASS.

If `a_condition_holding_an_unresolvable_reference_takes_both_branches` fails with one branch, the `Resolver` arm is not firing — check that `load_schema` on a `file://` URL that does not exist returns `Err` rather than hanging.

- [ ] **Step 5: Run the whole workspace and the wasm check**

Run the three commands from Task 1 Step 6. Expected: clean.

- [ ] **Step 6: Format and commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/mod.rs /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution add crates/taplo-common/src/schema/mod.rs crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution commit -m "$(cat <<'EOF'
feat(schema): decide a condition holding a ref

A condition containing a reference was refused outright, because a
subschema evaluated outside its document loses the document its pointers
name and rejects every instance, which would pick else forever.

Making the condition's references absolute removes the cause: the document
reaches the compiler through the resolver that already reads the cache.
The refusal is replaced by a narrower one, read from the validation errors
rather than guessed from the shape, because a reference resolves lazily and
an unfetched document otherwise reports a confident false.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Validate against the schema's own URL

`create_validator` never tells `jsonschema` which URL the schema came from, so the compilation scope is the root's `$id` or `json-schema:///`. Probed, three failures follow: with no `$id` a relative reference resolves against `json-schema:///`, misses the cache, and makes the whole `validate` call error with ``the scheme `json-schema` is not supported``; with a relative `$id` compilation fails outright; and only an absolute `$id` works. In every failing case the diagnostics handler logs and the editor shows a clean document.

**Files:**
- Modify: `crates/taplo-common/src/schema/mod.rs` (`add_validator`)
- Test: `crates/taplo-common/src/schema/tests.rs`

**Interfaces:**
- Consumes: `declared_draft(schema) -> DeclaredDraft`, `reference_url(base, reference)`.
- Produces: `add_validator(&self, schema_url: Url, schema: &Value)` — unchanged signature.

- [ ] **Step 1: Write the failing tests**

Append to `crates/taplo-common/src/schema/tests.rs`:

```rust
/// Every shape in the spec's end-to-end table, asserted of both halves at once.
/// A validation result of "no errors" is not acceptable: an unresolved
/// reference produces exactly that, so the type error has to be present and
/// the resolver errors absent.
#[tokio::test]
async fn traversal_and_validation_agree_on_every_reference_shape() {
    let shapes: &[(&str, Value)] = &[
        ("pointer", json!({ "$ref": "#/definitions/port" })),
        ("defs-pointer", json!({ "$ref": "#/$defs/port" })),
        ("relative", json!({ "$ref": "common.json#/definitions/port" })),
        (
            "absolute",
            json!({ "$ref": "file:///taplo-test/common.json#/definitions/port" }),
        ),
        ("anchor", json!({ "$ref": "#port" })),
        ("rescoped", json!({ "$ref": "#/definitions/wrapper" })),
    ];

    for (name, reference) in shapes {
        let (schemas, url) = seeded_documents(&[
            (
                "schema.json",
                json!({
                    "type": "object",
                    "properties": { "port": reference },
                    "definitions": {
                        "port": { "description": name, "type": "integer" },
                        "anchored": { "$id": "#port", "description": name, "type": "integer" },
                        "wrapper": { "$id": "defs/", "$ref": "port.json" }
                    },
                    "$defs": { "port": { "description": name, "type": "integer" } }
                }),
            ),
            (
                "common.json",
                json!({
                    "definitions": { "port": { "description": name, "type": "integer" } }
                }),
            ),
            (
                "defs/port.json",
                json!({ "description": name, "type": "integer" }),
            ),
        ])
        .await;

        let found = schemas
            .schemas_at_path(&url, &Value::Null, &"port".parse::<Keys>().unwrap())
            .await
            .unwrap_or_else(|e| panic!("traversal failed for `{name}`: {e}"));

        assert_eq!(descriptions(&found), [*name], "traversal, `{name}`");

        let errors = schemas
            .validate(&url, &json!({ "port": "not an integer" }))
            .await
            .unwrap_or_else(|e| panic!("validation failed for `{name}`: {e}"));

        assert!(
            errors.iter().any(|e| matches!(
                e.kind,
                jsonschema::error::ValidationErrorKind::Type { .. }
            )),
            "validation, `{name}`: expected a type error, got {errors:?}"
        );

        assert!(
            !errors.iter().any(|e| matches!(
                e.kind,
                jsonschema::error::ValidationErrorKind::Resolver { .. }
                    | jsonschema::error::ValidationErrorKind::InvalidReference { .. }
            )),
            "validation, `{name}`: a reference did not resolve, {errors:?}"
        );
    }
}

#[tokio::test]
async fn a_relative_root_id_does_not_break_compilation() {
    let (schemas, url) = seeded(json!({
        "$id": "schema.json",
        "type": "object",
        "properties": { "port": { "type": "integer" } }
    }))
    .await;

    let errors = schemas
        .validate(&url, &json!({ "port": "not an integer" }))
        .await
        .expect("compilation failed for a relative root $id");

    assert_eq!(errors.len(), 1);
}

#[tokio::test]
async fn a_draft_4_root_resolves_a_relative_reference() {
    let (schemas, url) = seeded_documents(&[
        (
            "schema.json",
            json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "type": "object",
                "properties": { "port": { "$ref": "common.json#/definitions/port" } }
            }),
        ),
        (
            "common.json",
            json!({ "definitions": { "port": { "type": "integer" } } }),
        ),
    ])
    .await;

    let errors = schemas
        .validate(&url, &json!({ "port": "not an integer" }))
        .await
        .expect("validation errored for a draft-4 root");

    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            jsonschema::error::ValidationErrorKind::Type { .. }
        )),
        "expected a type error, got {errors:?}"
    );
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls -- traversal_and_validation_agree_on_every_reference_shape a_relative_root_id_does_not_break_compilation a_draft_4_root_resolves_a_relative_reference`

Expected:
- `traversal_and_validation_agree_on_every_reference_shape` fails at the `relative` shape with ``validation failed for `relative`: the scheme `json-schema` is not supported``. Traversal passes every shape, since Tasks 2 to 4 landed.
- `a_relative_root_id_does_not_break_compilation` fails with `compilation failed for a relative root $id: invalid schema file:///taplo-test/schema.json`.
- `a_draft_4_root_resolves_a_relative_reference` fails the same way as the first.

- [ ] **Step 3: Make the root identifier absolute before compiling**

Replace `add_validator` in `crates/taplo-common/src/schema/mod.rs`:

```rust
    fn add_validator(
        &self,
        schema_url: Url,
        schema: &Value,
    ) -> Result<Arc<JSONSchema>, anyhow::Error> {
        let scoped = scoped_to(&schema_url, schema);
        let v = Arc::new(self.create_validator(scoped.as_ref().unwrap_or(schema))?);
        self.validators.lock().put(schema_url, v.clone());
        Ok(v)
    }
```

and add beside `declared_draft`:

```rust
/// A copy of `schema` whose root identifier is absolute against the URL it was
/// loaded from, or `None` when it already is.
///
/// `jsonschema` takes its compilation scope from the root identifier alone and
/// offers no way to set a base, so a schema without one resolves every relative
/// reference against `json-schema:///` — a scheme nothing can fetch — and a
/// schema with a relative one fails to compile at all. Both make the whole
/// `validate` call error, which reaches the reader as a document with no
/// diagnostics.
///
/// Draft 4 spells the keyword `id`, and that is the one `jsonschema` reads
/// under that draft, so the draft decides which is written.
fn scoped_to(schema_url: &Url, schema: &Value) -> Option<Value> {
    let keyword = match declared_draft(schema) {
        DeclaredDraft::Supported(Draft::Draft4) => "id",
        _ => "$id",
    };

    let scope = match schema[keyword].as_str() {
        Some(declared) if Url::parse(declared).is_ok() => return None,
        Some(declared) => reference_url(schema_url, declared)?,
        None => schema_url.clone(),
    };

    let mut scoped = schema.clone();
    scoped
        .as_object_mut()?
        .insert(keyword.to_owned(), Value::String(scope.into()));

    Some(scoped)
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls`

Expected: PASS.

If `traversal_and_validation_agree_on_every_reference_shape` fails at `anchor` on the validation side, check that the fixture's `$id: "#port"` sits under `definitions.anchored` — `jsonschema` indexes it wherever it is, but only if the root scope is absolute, which is what this task supplies.

- [ ] **Step 5: Run the whole workspace and the wasm check**

Run:
```
cargo check --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml --workspace --all-targets
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml --workspace
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-lsp --lib handlers::hover
cargo check --target wasm32-unknown-unknown --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-wasm/Cargo.toml
```

Expected: clean, with the two inherited `dead_code` warnings. `handlers::hover` gains one test over the baseline's 57 and `handlers::completion` gains one.

- [ ] **Step 6: Format and commit**

```bash
rustfmt --edition 2021 /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/mod.rs /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution add crates/taplo-common/src/schema/mod.rs crates/taplo-common/src/schema/tests.rs
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution commit -m "$(cat <<'EOF'
fix(schema): validate against the schema's own url

jsonschema takes its compilation scope from the root identifier and offers
no way to set a base, and taplo never wrote one. A schema with a relative
reference and no identifier therefore resolved it against json-schema:///,
and one with a relative identifier failed to compile; both made the whole
validate call error, which the editor renders as a clean document.

The root identifier is made absolute against the URL the schema was loaded
from, in the spelling the declared draft reads.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Verification

After Task 7, the branch is complete. Confirm the whole of it, from the worktree root:

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-ref-resolution status --short
cargo check --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml --workspace --all-targets
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml --workspace
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-common --features schema,reqwest,rustls-tls
cargo test --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/Cargo.toml -p taplo-lsp --lib handlers::hover
cargo check --target wasm32-unknown-unknown --manifest-path /home/lev/Git/lev/taplo-wt/schema-ref-resolution/crates/taplo-wasm/Cargo.toml
```

`git status` must be empty. The workspace check and test must be clean. Every test in `taplo-common`'s schema suite passes, and it is substantially larger than the baseline's 39; `handlers::hover` grows from 57 to 58 and `handlers::completion` by one. No test from the baseline is removed or weakened, with one deliberate exception: `a_condition_holding_a_nested_reference_takes_both_branches` becomes `a_condition_holding_a_nested_reference_decides_its_branch` in Task 6.

Nobody follows this feature set, so three things belong on the tracking issue when it is updated:

- `collect_schemas` discards an `allOf` carrier's own annotations, which is what `schemars` emits for every documented non-`Option` field. The fix is to give it the composed-`allOf` merge `collect_child_schemas` has, after settling what that merge does to arrays — `json_value_merge` concatenates them.
- `$anchor` resolves in neither traversal nor validation. It lands with the `jsonschema` upgrade that brings `unevaluatedItems`.
- A `$ref` to an embedded resource — a subschema whose `$id` gives it its own URL inside a larger document, referenced by that URL — resolves in validation and not in traversal, which fetches the URL and finds no document.
