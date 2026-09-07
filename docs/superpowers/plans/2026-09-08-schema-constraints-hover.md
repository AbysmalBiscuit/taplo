# JSON Schema constraints in hover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render the JSON Schema constraint keywords `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`, `multipleOf`, `minLength`, `maxLength`, `pattern`, `format`, `contentMediaType`, `contentEncoding`, `minItems`, `maxItems`, `uniqueItems`, `minProperties` and `maxProperties` in the Taplo language server's key hover text.

**Architecture:** Each keyword group becomes a `Fact` pushed in `key_hover_sections`. Paired bounds collapse onto one line through a shared `bounds_fact`, and a type filter drops any keyword the schema's declared `type` makes vacuous. `HoverSections::render` is unchanged: the wall of text is prevented by producing fewer facts, not by restructuring the list.

**Tech Stack:** Rust 2021, `serde_json` 1.0.113, `itertools`, the workspace-local `taplo` and `lsp-async-stub` crates, `tokio` for the existing `#[tokio::test]` handler fixture.

**Spec:** `docs/superpowers/specs/2026-09-08-schema-constraints-hover.md`

## Global Constraints

- Work only in `/home/lev/Git/lev/taplo-wt/schema-constraints-hover` on branch `feat/schema-constraints-hover`. Use absolute paths; never `cd` first; use `git -C /home/lev/Git/lev/taplo-wt/schema-constraints-hover ...`. Never `git stash`; read another revision with `git show REV:path`.
- Sibling worktrees under `/home/lev/Git/lev/taplo-wt/` belong to other agents. Do not read from, write to, or run commands in them.
- Use `rg`, not `grep`; `fd`, not `find`. `rg -r` means `--replace`, so line numbers are `rg -n`.
- Conventional Commits, imperative mood, lowercase after the colon, subject at most 50 characters. Body only where the change needs context.
- Comments are timeless: state what the code does and the non-obvious why. Never "this PR", "now we", "previously"; never reference a plan task or an issue number.
- Only the literal boolean `true` counts for `uniqueItems` and for the draft-4 spellings of `exclusiveMinimum` and `exclusiveMaximum`; the existing `flag` helper is the one definition of that.
- Only a JSON number counts for `minimum`, `maximum`, `multipleOf`, `minLength`, `maxLength`, `minItems`, `maxItems`, `minProperties`, `maxProperties` and the draft-6 spelling of the exclusive bounds. Only a non-empty JSON string counts for `pattern`, `format`, `contentMediaType` and `contentEncoding`.
- Every file this plan touches is `crates/taplo-lsp/src/handlers/hover.rs`. Nothing else in the workspace changes.
- Run `cargo fmt -p taplo-lsp` after each implementation step. `cargo fmt --check` is not clean at the branch point for `taplo-common`, `taplo-wasm` and `taplo`; leave those alone.
- Every task ends with a commit. Do not push, do not open a pull request.

**Verification command set** (referred to below as "the full check"), run from `/home/lev/Git/lev/taplo-wt/schema-constraints-hover`:

```
cargo check --workspace --all-targets
cargo test --workspace
cargo check --target wasm32-unknown-unknown --manifest-path crates/taplo-wasm/Cargo.toml
cargo run -p taplo-cli -- fmt --check
```

All four pass at the branch point `5a49d25`. A failure that is not in `taplo-lsp` is inherited, not caused by this plan.

---

## File Structure

- `crates/taplo-lsp/src/handlers/hover.rs` — the only file this plan modifies. It gains `Bound`, `bounds_fact`, `numeric_bounds`, `inclusive_bounds`, `string_fact`, `admits_type`, ten `Fact` contributors, four `if admits_type(...)` blocks in `key_hover_sections`, and tests in its existing `mod tests`.

The file already holds `Fact`, `HoverSections`, `render`, `code_span`, `flag`, `default_fact`, `examples_fact`, `schema_docs`, `key_hover_sections` and a `mod tests` with a `hover_at` fixture and a `key_hover_for` helper. Nothing in it is restructured; every task appends.

Contributors are placed above `key_hover_sections` and below `examples_fact`, in the order they are pushed. Tests are appended to the end of `mod tests`.

### The existing pieces every task depends on

```rust
/// A labelled hover line: `Default` with one value, `Examples` with several,
/// `Read-only` with none.
struct Fact {
    label: &'static str,
    values: Vec<String>,
}
```

`HoverSections::render` turns each `Fact` into `- {label}: {values joined with ", "}` when `values` is non-empty and `- {label}` when it is empty, applying `code_span` to every value, and joins the facts with a single `\n`. It joins the banner, docs and fact blocks with `\n\n` and drops empty blocks.

`fn flag(schema: &Value, keyword: &str) -> bool` returns `schema[keyword].as_bool().unwrap_or(false)`.

`fn code_span(text: &str) -> String` widens the fence past the longest backtick run in `text` and pads with a space on each side when `text` begins or ends with a backtick.

In `mod tests`:

```rust
/// Wraps a property schema in an object schema and hovers its key.
async fn key_hover_for(property: serde_json::Value) -> Option<String> {
    let schema = json!({ "type": "object", "properties": { "port": property } });
    hover_at(schema, "port = 8080\n", 1).await
}
```

Every handler test in this plan calls `key_hover_for` and must be `#[tokio::test]`, because `NativeEnvironment::new` requires an active tokio runtime.

---

## Task 1: Numeric constraints

**Files:**
- Modify: `crates/taplo-lsp/src/handlers/hover.rs`

**Interfaces:**
- Consumes: `Fact`, `flag`, `key_hover_sections`, and `key_hover_for` in `mod tests`, all already present.
- Produces, for tasks 2 and 3:
  - `struct Bound { value: Number, exclusive: bool }`
  - `fn bounds_fact(label: &'static str, lower: Vec<Bound>, upper: Vec<Bound>) -> Option<Fact>`
  - `fn admits_type(schema: &Value, wanted: &str) -> bool`

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/taplo-lsp/src/handlers/hover.rs`:

```rust
#[tokio::test]
async fn key_hover_renders_a_two_sided_range() {
    assert_eq!(
        key_hover_for(json!({ "type": "integer", "minimum": 1, "maximum": 10 }))
            .await
            .unwrap(),
        "- Range: `>= 1`, `<= 10`"
    );
}

#[tokio::test]
async fn key_hover_renders_a_one_sided_range() {
    assert_eq!(
        key_hover_for(json!({ "type": "integer", "minimum": 1 }))
            .await
            .unwrap(),
        "- Range: `>= 1`"
    );

    assert_eq!(
        key_hover_for(json!({ "type": "number", "maximum": 2.5 }))
            .await
            .unwrap(),
        "- Range: `<= 2.5`"
    );
}

#[tokio::test]
async fn key_hover_renders_numeric_exclusive_bounds() {
    assert_eq!(
        key_hover_for(json!({
            "type": "integer",
            "exclusiveMinimum": 0,
            "exclusiveMaximum": 10
        }))
        .await
        .unwrap(),
        "- Range: `> 0`, `< 10`"
    );
}

#[tokio::test]
async fn key_hover_honors_the_draft_4_boolean_exclusive_bounds() {
    assert_eq!(
        key_hover_for(json!({ "type": "integer", "minimum": 1, "exclusiveMinimum": true }))
            .await
            .unwrap(),
        "- Range: `> 1`"
    );

    assert_eq!(
        key_hover_for(json!({ "type": "integer", "maximum": 10, "exclusiveMaximum": true }))
            .await
            .unwrap(),
        "- Range: `< 10`"
    );

    assert_eq!(
        key_hover_for(json!({ "type": "integer", "minimum": 1, "exclusiveMinimum": false }))
            .await
            .unwrap(),
        "- Range: `>= 1`"
    );
}

#[tokio::test]
async fn key_hover_ignores_a_boolean_exclusive_bound_with_no_neighbour() {
    assert_eq!(
        key_hover_for(json!({ "type": "integer", "exclusiveMinimum": true })).await,
        None
    );
}

#[tokio::test]
async fn key_hover_renders_every_bound_written_on_one_side() {
    assert_eq!(
        key_hover_for(json!({ "type": "integer", "minimum": 5, "exclusiveMinimum": 0 }))
            .await
            .unwrap(),
        "- Range: `>= 5`, `> 0`"
    );
}

#[tokio::test]
async fn key_hover_collapses_equal_inclusive_bounds() {
    assert_eq!(
        key_hover_for(json!({ "type": "integer", "minimum": 5, "maximum": 5 }))
            .await
            .unwrap(),
        "- Range: `5`"
    );
}

#[tokio::test]
async fn key_hover_does_not_collapse_an_integer_against_a_float() {
    assert_eq!(
        key_hover_for(json!({ "type": "number", "minimum": 40, "maximum": 40.0 }))
            .await
            .unwrap(),
        "- Range: `>= 40`, `<= 40.0`"
    );
}

#[tokio::test]
async fn key_hover_renders_multiple_of() {
    assert_eq!(
        key_hover_for(json!({ "type": "integer", "multipleOf": 5 }))
            .await
            .unwrap(),
        "- Multiple of: `5`"
    );
}

#[tokio::test]
async fn key_hover_ignores_numeric_keywords_of_the_wrong_shape() {
    for property in [
        json!({ "type": "integer", "minimum": "1" }),
        json!({ "type": "integer", "multipleOf": "5" }),
        json!({ "type": "integer", "exclusiveMinimum": "0" }),
    ] {
        assert_eq!(key_hover_for(property).await, None);
    }
}

#[tokio::test]
async fn key_hover_drops_numeric_keywords_the_declared_type_makes_dead() {
    for property in [
        json!({ "type": "string", "minimum": 1 }),
        json!({ "type": "string", "multipleOf": 5 }),
        json!({ "type": "array", "maximum": 10 }),
    ] {
        assert_eq!(key_hover_for(property).await, None);
    }
}

#[tokio::test]
async fn key_hover_renders_numeric_constraints_for_an_untyped_schema() {
    assert_eq!(
        key_hover_for(json!({ "minimum": 1 })).await.unwrap(),
        "- Range: `>= 1`"
    );
}

#[tokio::test]
async fn key_hover_renders_numeric_constraints_for_a_type_union() {
    assert_eq!(
        key_hover_for(json!({ "type": ["integer", "null"], "minimum": 1 }))
            .await
            .unwrap(),
        "- Range: `>= 1`"
    );
}

#[tokio::test]
async fn key_hover_orders_constraints_between_values_and_access() {
    let content = key_hover_for(json!({
        "type": "integer",
        "description": "The port.",
        "default": 8080,
        "minimum": 1,
        "maximum": 65535,
        "readOnly": true
    }))
    .await
    .unwrap();

    assert_eq!(
        content,
        "The port.\n\n- Default: `8080`\n- Range: `>= 1`, `<= 65535`\n- Read-only"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p taplo-lsp --lib handlers::hover 2>&1 | tail -30`

Expected: FAIL at runtime, not at compile time. Every test that unwraps panics with `called 'Option::unwrap()' on a 'None' value`, because no constraint keyword is read yet and a schema carrying only constraints renders nothing. `key_hover_ignores_a_boolean_exclusive_bound_with_no_neighbour`, `key_hover_ignores_numeric_keywords_of_the_wrong_shape` and `key_hover_drops_numeric_keywords_the_declared_type_makes_dead` pass already, because nothing reads those keywords yet; they are kept as regression cover. If the crate fails to compile instead, the failure is a typo in the test code rather than the behavior under test — fix it and re-run before continuing.

- [ ] **Step 3: Write the implementation**

Add `use serde_json::{Number, Value};` in place of the existing `use serde_json::Value;` at the top of the file.

Insert after `examples_fact` and before `schema_docs`:

```rust
/// One end of a range: the bound and whether the schema excludes it.
struct Bound {
    value: Number,
    exclusive: bool,
}

impl Bound {
    /// The comparison a reader sees, given the operators for this side.
    fn render(&self, inclusive: &str, exclusive: &str) -> String {
        let operator = if self.exclusive { exclusive } else { inclusive };
        format!("{operator} {}", self.value)
    }
}

/// Renders the bounds on a quantity as one fact, so that a lower and an upper
/// bound share a line rather than taking one each.
///
/// A pair of inclusive bounds on the same value collapses to the bare value,
/// which is how a fixed size reads best. The collapse needs exactly one bound
/// per side, since a schema writing two bounds on one side has stated two
/// separate comparisons.
fn bounds_fact(label: &'static str, lower: Vec<Bound>, upper: Vec<Bound>) -> Option<Fact> {
    if lower.is_empty() && upper.is_empty() {
        return None;
    }

    if let ([low], [high]) = (lower.as_slice(), upper.as_slice()) {
        if !low.exclusive && !high.exclusive && low.value == high.value {
            return Some(Fact {
                label,
                values: vec![low.value.to_string()],
            });
        }
    }

    let values = lower
        .iter()
        .map(|bound| bound.render(">=", ">"))
        .chain(upper.iter().map(|bound| bound.render("<=", "<")))
        .collect();

    Some(Fact { label, values })
}

/// Reads one side of a numeric range.
///
/// Both spellings of an exclusive bound are honored, told apart by shape: a
/// number in the exclusive keyword is the bound itself, while a boolean beside
/// the inclusive keyword is draft 4's modifier on that bound. A schema may
/// write both, and both render.
fn numeric_bounds(schema: &Value, inclusive: &str, exclusive: &str) -> Vec<Bound> {
    let mut bounds = Vec::new();

    if let Some(value) = schema[inclusive].as_number() {
        bounds.push(Bound {
            value: value.clone(),
            exclusive: flag(schema, exclusive),
        });
    }

    if let Some(value) = schema[exclusive].as_number() {
        bounds.push(Bound {
            value: value.clone(),
            exclusive: true,
        });
    }

    bounds
}

/// The range a numeric schema admits.
fn range_fact(schema: &Value) -> Option<Fact> {
    bounds_fact(
        "Range",
        numeric_bounds(schema, "minimum", "exclusiveMinimum"),
        numeric_bounds(schema, "maximum", "exclusiveMaximum"),
    )
}

/// The step a numeric schema admits.
fn multiple_of_fact(schema: &Value) -> Option<Fact> {
    Some(Fact {
        label: "Multiple of",
        values: vec![schema["multipleOf"].as_number()?.to_string()],
    })
}

/// Whether a schema's declared `type` admits instances of `wanted`, and so
/// whether a keyword constraining `wanted` can ever fire.
///
/// Every constraint keyword is defined conditionally on the instance type, so
/// one written against a type the schema does not admit is vacuous and hover
/// leaves it out. A schema that declares no type, or declares one in a shape
/// the specification does not describe, admits everything: the author wrote
/// the keyword, and hover has nothing better to go on.
fn admits_type(schema: &Value, wanted: &str) -> bool {
    fn matches(declared: &str, wanted: &str) -> bool {
        declared == wanted || (wanted == "number" && declared == "integer")
    }

    match &schema["type"] {
        Value::String(declared) => matches(declared, wanted),
        Value::Array(declared) => {
            let mut named = declared.iter().filter_map(Value::as_str).peekable();
            named.peek().is_none() || named.any(|declared| matches(declared, wanted))
        }
        _ => true,
    }
}
```

In `key_hover_sections`, insert between the `examples_fact` line and the `readOnly` block:

```rust
    if admits_type(schema, "number") {
        sections.facts.extend(range_fact(schema));
        sections.facts.extend(multiple_of_fact(schema));
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo fmt -p taplo-lsp && cargo test -p taplo-lsp --lib handlers::hover`

Expected: PASS, all tests in the module including the pre-existing ones.

- [ ] **Step 5: Run the full check**

Run each command from the "Verification command set" above.
Expected: all four clean.

- [ ] **Step 6: Commit**

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-constraints-hover add crates/taplo-lsp/src/handlers/hover.rs
git -C /home/lev/Git/lev/taplo-wt/schema-constraints-hover commit -m "feat(lsp): render numeric constraints in hover

A bounded pair shares one line, written as the comparisons that apply,
so that a range costs one bullet rather than two. Both spellings of an
exclusive bound are honored, told apart by shape rather than by a draft
the renderer does not have.

admits_type drops a keyword the declared type makes vacuous, which is
what keeps the popup short as the remaining keyword groups land on it."
```

---

## Task 2: String constraints

**Files:**
- Modify: `crates/taplo-lsp/src/handlers/hover.rs`

**Interfaces:**
- Consumes from Task 1: `Bound`, `bounds_fact`, `admits_type`.
- Produces, for Task 3: `fn inclusive_bounds(schema: &Value, keyword: &str) -> Vec<Bound>`.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests`:

```rust
#[tokio::test]
async fn key_hover_renders_a_string_length() {
    assert_eq!(
        key_hover_for(json!({ "type": "string", "minLength": 1, "maxLength": 128 }))
            .await
            .unwrap(),
        "- Length: `>= 1`, `<= 128`"
    );

    assert_eq!(
        key_hover_for(json!({ "type": "string", "maxLength": 128 }))
            .await
            .unwrap(),
        "- Length: `<= 128`"
    );
}

#[tokio::test]
async fn key_hover_collapses_a_fixed_string_length() {
    assert_eq!(
        key_hover_for(json!({ "type": "string", "minLength": 40, "maxLength": 40 }))
            .await
            .unwrap(),
        "- Length: `40`"
    );
}

#[tokio::test]
async fn key_hover_renders_a_pattern() {
    assert_eq!(
        key_hover_for(json!({ "type": "string", "pattern": r"^v\d+$" }))
            .await
            .unwrap(),
        r"- Pattern: `^v\d+$`"
    );
}

#[tokio::test]
async fn key_hover_fences_a_pattern_containing_backticks() {
    assert_eq!(
        key_hover_for(json!({ "type": "string", "pattern": "a`b" }))
            .await
            .unwrap(),
        "- Pattern: ``a`b``"
    );

    assert_eq!(
        key_hover_for(json!({ "type": "string", "pattern": "`x" }))
            .await
            .unwrap(),
        "- Pattern: `` `x ``"
    );
}

#[tokio::test]
async fn key_hover_renders_a_format_whether_or_not_taplo_enforces_it() {
    assert_eq!(
        key_hover_for(json!({ "type": "string", "format": "semver" }))
            .await
            .unwrap(),
        "- Format: `semver`"
    );

    assert_eq!(
        key_hover_for(json!({ "type": "string", "format": "uri-template" }))
            .await
            .unwrap(),
        "- Format: `uri-template`"
    );
}

#[tokio::test]
async fn key_hover_renders_content_annotations() {
    assert_eq!(
        key_hover_for(json!({
            "type": "string",
            "contentMediaType": "application/json",
            "contentEncoding": "base64"
        }))
        .await
        .unwrap(),
        "- Media type: `application/json`\n- Encoding: `base64`"
    );
}

#[tokio::test]
async fn key_hover_ignores_string_keywords_of_the_wrong_shape() {
    for property in [
        json!({ "type": "string", "pattern": 12 }),
        json!({ "type": "string", "format": 12 }),
        json!({ "type": "string", "pattern": "" }),
        json!({ "type": "string", "format": "" }),
        json!({ "type": "string", "contentMediaType": "" }),
        json!({ "type": "string", "contentEncoding": "" }),
        json!({ "type": "string", "minLength": "1" }),
    ] {
        assert_eq!(key_hover_for(property).await, None);
    }
}

#[tokio::test]
async fn key_hover_drops_string_keywords_the_declared_type_makes_dead() {
    for property in [
        json!({ "type": "integer", "minLength": 5 }),
        json!({ "type": "integer", "pattern": "^a$" }),
        json!({ "type": "integer", "format": "email" }),
        json!({ "type": "integer", "contentEncoding": "base64" }),
    ] {
        assert_eq!(key_hover_for(property).await, None);
    }
}

#[tokio::test]
async fn key_hover_renders_string_constraints_for_a_type_union() {
    assert_eq!(
        key_hover_for(json!({ "type": ["string", "null"], "minLength": 1 }))
            .await
            .unwrap(),
        "- Length: `>= 1`"
    );
}

#[tokio::test]
async fn key_hover_renders_a_documented_string_key_as_five_bullets() {
    let content = key_hover_for(json!({
        "type": "string",
        "description": "The image tag to deploy.",
        "default": "latest",
        "examples": ["v1.2.3", "latest"],
        "minLength": 1,
        "maxLength": 128,
        "pattern": r"^[\w.-]+$",
        "format": "semver"
    }))
    .await
    .unwrap();

    assert_eq!(
        content,
        concat!(
            "The image tag to deploy.\n",
            "\n",
            "- Default: `\"latest\"`\n",
            "- Examples: `\"v1.2.3\"`, `\"latest\"`\n",
            "- Length: `>= 1`, `<= 128`\n",
            r"- Pattern: `^[\w.-]+$`",
            "\n",
            "- Format: `semver`"
        )
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p taplo-lsp --lib handlers::hover 2>&1 | tail -40`

Expected: FAIL. `key_hover_renders_a_string_length` and its neighbours panic on `unwrap` of `None`; `key_hover_renders_a_documented_string_key_as_five_bullets` fails on an assertion showing the three constraint bullets missing. `key_hover_ignores_string_keywords_of_the_wrong_shape` and `key_hover_drops_string_keywords_the_declared_type_makes_dead` pass already, because nothing reads those keywords yet — that is expected and they are kept as regression cover.

- [ ] **Step 3: Write the implementation**

Insert after `multiple_of_fact` and before `admits_type`:

```rust
/// Reads a keyword that bounds a size or a count, which no draft spells as
/// exclusive.
///
/// The result is a `Vec` of at most one so that every caller of `bounds_fact`
/// has the same shape.
fn inclusive_bounds(schema: &Value, keyword: &str) -> Vec<Bound> {
    schema[keyword]
        .as_number()
        .map(|value| Bound {
            value: value.clone(),
            exclusive: false,
        })
        .into_iter()
        .collect()
}

/// A fact whose only value is a string the schema states verbatim, such as a
/// regular expression or a format name.
///
/// An empty string constrains nothing, and an empty code span is not a code
/// span, so it contributes no fact.
fn string_fact(schema: &Value, label: &'static str, keyword: &str) -> Option<Fact> {
    let value = schema[keyword].as_str().filter(|value| !value.is_empty())?;

    Some(Fact {
        label,
        values: vec![value.to_owned()],
    })
}
```

In `key_hover_sections`, insert after the `admits_type(schema, "number")` block:

```rust
    if admits_type(schema, "string") {
        sections.facts.extend(bounds_fact(
            "Length",
            inclusive_bounds(schema, "minLength"),
            inclusive_bounds(schema, "maxLength"),
        ));
        sections.facts.extend(string_fact(schema, "Pattern", "pattern"));
        sections.facts.extend(string_fact(schema, "Format", "format"));
        sections
            .facts
            .extend(string_fact(schema, "Media type", "contentMediaType"));
        sections
            .facts
            .extend(string_fact(schema, "Encoding", "contentEncoding"));
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo fmt -p taplo-lsp && cargo test -p taplo-lsp --lib handlers::hover`
Expected: PASS.

- [ ] **Step 5: Run the full check**

Run each command from the "Verification command set".
Expected: all four clean.

- [ ] **Step 6: Commit**

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-constraints-hover add crates/taplo-lsp/src/handlers/hover.rs
git -C /home/lev/Git/lev/taplo-wt/schema-constraints-hover commit -m "feat(lsp): render string constraints in hover

Hover states what the schema declares and does not mark which formats
the validator enforces: whether a given format asserts depends on the
format name and the declared draft together, and a label that promised
enforcement would need that truth table to stay honest.

contentMediaType and contentEncoding are annotations from 2019-09 on and
produce no diagnostic under those drafts, so hover is the only channel
that carries them."
```

---

## Task 3: Array and object constraints

**Files:**
- Modify: `crates/taplo-lsp/src/handlers/hover.rs`

**Interfaces:**
- Consumes from Task 1: `bounds_fact`, `admits_type`, `flag`, `Fact`.
- Consumes from Task 2: `inclusive_bounds`.
- Produces: nothing new. This task adds contributors only.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests`:

```rust
#[tokio::test]
async fn key_hover_renders_array_constraints() {
    assert_eq!(
        key_hover_for(json!({
            "type": "array",
            "minItems": 1,
            "maxItems": 3,
            "uniqueItems": true
        }))
        .await
        .unwrap(),
        "- Items: `>= 1`, `<= 3`\n- Unique items"
    );
}

#[tokio::test]
async fn key_hover_ignores_a_unique_items_that_is_not_true() {
    for unique in [json!(false), json!("yes"), json!(1)] {
        assert_eq!(
            key_hover_for(json!({ "type": "array", "uniqueItems": unique })).await,
            None
        );
    }
}

#[tokio::test]
async fn key_hover_renders_object_constraints() {
    assert_eq!(
        key_hover_for(json!({ "type": "object", "minProperties": 1, "maxProperties": 5 }))
            .await
            .unwrap(),
        "- Properties: `>= 1`, `<= 5`"
    );
}

#[tokio::test]
async fn key_hover_drops_container_keywords_the_declared_type_makes_dead() {
    for property in [
        json!({ "type": "integer", "minItems": 5 }),
        json!({ "type": "integer", "uniqueItems": true }),
        json!({ "type": "string", "minProperties": 5 }),
        json!({ "type": "object", "minItems": 1 }),
    ] {
        assert_eq!(key_hover_for(property).await, None);
    }
}

#[tokio::test]
async fn key_hover_renders_every_constraint_an_untyped_schema_writes() {
    let content = key_hover_for(json!({
        "minimum": 1,
        "multipleOf": 2,
        "minLength": 1,
        "pattern": "^a$",
        "format": "email",
        "contentMediaType": "text/plain",
        "contentEncoding": "base64",
        "minItems": 1,
        "uniqueItems": true,
        "minProperties": 1
    }))
    .await
    .unwrap();

    assert_eq!(
        content,
        concat!(
            "- Range: `>= 1`\n",
            "- Multiple of: `2`\n",
            "- Length: `>= 1`\n",
            "- Pattern: `^a$`\n",
            "- Format: `email`\n",
            "- Media type: `text/plain`\n",
            "- Encoding: `base64`\n",
            "- Items: `>= 1`\n",
            "- Unique items\n",
            "- Properties: `>= 1`"
        )
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p taplo-lsp --lib handlers::hover 2>&1 | tail -40`

Expected: FAIL. `key_hover_renders_array_constraints` and `key_hover_renders_object_constraints` panic on `unwrap` of `None`; `key_hover_renders_every_constraint_an_untyped_schema_writes` fails on an assertion missing the last three bullets. `key_hover_ignores_a_unique_items_that_is_not_true` and `key_hover_drops_container_keywords_the_declared_type_makes_dead` pass already and are kept as regression cover.

- [ ] **Step 3: Write the implementation**

In `key_hover_sections`, insert after the `admits_type(schema, "string")` block and before the `readOnly` block:

```rust
    if admits_type(schema, "array") {
        sections.facts.extend(bounds_fact(
            "Items",
            inclusive_bounds(schema, "minItems"),
            inclusive_bounds(schema, "maxItems"),
        ));

        if flag(schema, "uniqueItems") {
            sections.facts.push(Fact {
                label: "Unique items",
                values: Vec::new(),
            });
        }
    }

    if admits_type(schema, "object") {
        sections.facts.extend(bounds_fact(
            "Properties",
            inclusive_bounds(schema, "minProperties"),
            inclusive_bounds(schema, "maxProperties"),
        ));
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo fmt -p taplo-lsp && cargo test -p taplo-lsp --lib handlers::hover`
Expected: PASS.

- [ ] **Step 5: Update the `Fact::values` doc comment**

The comment on the `values` field of `Fact` reads "Rendered TOML literals", which no longer covers comparisons, regular expressions and format names. Replace that field's comment only, leaving the struct's own doc comment and the `label` field untouched:

```rust
    /// Rendered values: TOML literals, comparisons, regular expressions and
    /// format names. `HoverSections::render` fences each as a code span, so a
    /// contributor never writes a backtick itself.
    values: Vec<String>,
```

- [ ] **Step 6: Run the full check**

Run each command from the "Verification command set".
Expected: all four clean.

- [ ] **Step 7: Commit**

```bash
git -C /home/lev/Git/lev/taplo-wt/schema-constraints-hover add crates/taplo-lsp/src/handlers/hover.rs
git -C /home/lev/Git/lev/taplo-wt/schema-constraints-hover commit -m "feat(lsp): render array and object constraints"
```

---

## Verification against the spec's acceptance criteria

| Criterion | Covered by |
|---|---|
| 1 | `key_hover_renders_a_two_sided_range`, `key_hover_collapses_equal_inclusive_bounds` |
| 2 | `key_hover_renders_numeric_exclusive_bounds`, `key_hover_renders_a_one_sided_range` |
| 3 | `key_hover_honors_the_draft_4_boolean_exclusive_bounds`, `key_hover_ignores_a_boolean_exclusive_bound_with_no_neighbour` |
| 4 | `key_hover_renders_multiple_of` |
| 5 | `key_hover_renders_every_bound_written_on_one_side` |
| 6 | `key_hover_renders_a_string_length`, `key_hover_collapses_a_fixed_string_length` |
| 7 | `key_hover_renders_a_pattern`, `key_hover_fences_a_pattern_containing_backticks`, `key_hover_ignores_string_keywords_of_the_wrong_shape` |
| 8 | `key_hover_renders_a_format_whether_or_not_taplo_enforces_it` |
| 9 | `key_hover_renders_content_annotations` |
| 10 | `key_hover_renders_array_constraints`, `key_hover_ignores_a_unique_items_that_is_not_true` |
| 11 | `key_hover_renders_object_constraints` |
| 12 | `key_hover_drops_numeric_keywords_the_declared_type_makes_dead`, `key_hover_drops_string_keywords_the_declared_type_makes_dead`, `key_hover_drops_container_keywords_the_declared_type_makes_dead` |
| 13 | `key_hover_renders_numeric_constraints_for_a_type_union`, `key_hover_renders_string_constraints_for_a_type_union`, `key_hover_renders_every_constraint_an_untyped_schema_writes` |
| 14 | `key_hover_ignores_numeric_keywords_of_the_wrong_shape`, `key_hover_ignores_string_keywords_of_the_wrong_shape` |
| 15 | `key_hover_renders_a_documented_string_key_as_five_bullets` |
| 16 | The full check, run at the end of every task |

`key_hover_orders_constraints_between_values_and_access` and `key_hover_does_not_collapse_an_integer_against_a_float` carry no criterion number; the first pins the fact order the spec's "Facts" section states, and the second pins the `serde_json::Number` equality rule under "Bounds".
