# TOML version gating Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the formatter turning a valid TOML 1.0 file into one that does not parse, by making the TOML version an explicit, resolvable formatter option instead of an accident of which inline-table flags happen to be on.

**Architecture:** A `TomlVersion` option joins the formatter's option surface, defaulting to `Auto`. `format_impl` is the one chokepoint every public entry point passes through; it resolves `Auto` against the document once (directive, then configured value, then detection, then 1.0) and clamps the inline-table options before any formatting happens. Under 1.0 the formatter cannot emit a newline between braces: expansion is off and collapse is on. Detection reads the syntax tree for an `INLINE_TABLE` containing a `NEWLINE`, which is the only TOML 1.1 construct this formatter is capable of introducing.

**Tech Stack:** Rust, `rowan`, `serde`, `schemars`, the `create_options!` macro.

**Spec:** `docs/superpowers/specs/2026-09-09-toml-version-gating.md`

## Global Constraints

- Work only in the worktree `/home/lev/Git/lev/taplo-wt/toml-version-gating`, on branch `feat/toml-version-gating`. Use `git -C /home/lev/Git/lev/taplo-wt/toml-version-gating …` and absolute paths in every command. Never `cd` first. Never `git stash`; read another revision with `git show REV:path`.
- Commit, do not push. No `git push`, no pull request, no issue edits.
- Conventional Commits, imperative mood, lowercase after the colon, subject ≤ 50 characters. A body only where the change needs context. End every commit message with `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.
- Comments are timeless: what the code does and the non-obvious why. Never "this PR", "now we", "previously", never a task or issue number, never RED/GREEN narration.
- `rg` not `grep`, `fd` not `find`. `rg -r` means `--replace`, so line numbers are `rg -n`.
- Format only the files you touch: `rustfmt --edition 2021 <file>` on the exact files changed. Do not run `cargo fmt` across the workspace.
- Markdown paragraphs are written as one line each. Do not hand-wrap prose at a column.
- The baseline at `a9bc0c3` is green: `cargo test -p taplo --all-features` passes 104 lib tests and 1 doc test.
- Run the crate's tests with `cargo test --manifest-path /home/lev/Git/lev/taplo-wt/toml-version-gating/Cargo.toml -p taplo --all-features`, and the CLI's with `-p taplo-cli --test multiline_inline_tables`.
- Where a step says "PASS", it means every test that passed before the task still passes, plus the ones the task adds. Do not treat a total count as the assertion.
- The three inline-table options (`inline_table_expand`, `inline_table_auto_collapse`, `inline_table_trailing_comma`) keep their current defaults. The version gate clamps them at format time; it never rewrites the defaults.

---

### Task 1: A resolvable TOML version

Add the type and the resolution rule. No formatting behavior changes in this task: the resolved version is computed and returned, and nothing consumes it yet. This lands first so the resolution rule is settled and tested before anything depends on it.

**Files:**
- Create: `crates/taplo/src/formatter/version.rs`
- Modify: `crates/taplo/src/formatter/mod.rs` (add `mod version;`, re-export, add the option field)
- Test: `crates/taplo/src/formatter/version.rs` (a `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub enum TomlVersion { Auto, V1_0, V1_1 }`, `Default = Auto`.
- Produces: `pub enum ResolvedVersion { V1_0, V1_1 }`.
- Produces: `pub(crate) fn resolve_version(root: &SyntaxNode, configured: TomlVersion) -> ResolvedVersion`.
- Produces: `Options.toml_version: TomlVersion`, added to the `create_options!` block.

- [ ] **Step 1: Write the failing tests**

In `crates/taplo/src/formatter/version.rs`, a test module that parses a source with `crate::parser::parse(src).into_syntax()` and asserts `resolve_version(&root, configured)`:

| Source | Configured | Expected |
|---|---|---|
| `a = { x = 1 }` | `Auto` | `V1_0` |
| `a = {\n  x = 1,\n}` | `Auto` | `V1_1` |
| `a = { x = 1 }` | `V1_1` | `V1_1` |
| `a = {\n  x = 1,\n}` | `V1_0` | `V1_0` |
| `#:toml-version 1.1\na = { x = 1 }` | `Auto` | `V1_1` |
| `#:toml-version 1.0\na = {\n  x = 1,\n}` | `Auto` | `V1_0` |
| `#:toml-version 1.0\na = { x = 1 }` | `V1_1` | `V1_0` (directive outranks configured) |
| `#:toml-version 2.0\na = { x = 1 }` | `Auto` | `V1_0` (unparseable is inert) |
| `#:toml-version banana\na = {\n  x = 1,\n}` | `Auto` | `V1_1` (inert, detection still runs) |
| `a = 1\n#:toml-version 1.1\nb = { x = 1 }` | `Auto` | `V1_0` (directive not at the top) |
| `# toml-version 1.1\na = { x = 1 }` | `Auto` | `V1_0` (a plain comment is not a directive) |
| `a = [{ x = 1 }, {\n  y = 2,\n}]` | `Auto` | `V1_1` (detection reaches a nested table) |

Also assert `TomlVersion::from_str`: `"1.0"`, `"1.1"`, and `"auto"` parse; `"2.0"` and `""` are errors.

**Verify:** `cargo test -p taplo --all-features version::` → FAIL (the module does not exist).

- [ ] **Step 2: Implement**

`TomlVersion` derives `Debug, Clone, Copy, Eq, PartialEq, Default`, plus `Serialize, Deserialize` under `feature = "serde"` and `JsonSchema` under `feature = "schema"`, matching how `Options` is gated in `mod.rs`. Serde representation: `"auto"`, `"1.0"`, `"1.1"`. It implements `FromStr` over the same three strings, because `create_options!`'s `update_from_str` parses every option value from a string, and `Display` as the inverse.

`resolve_version` in order:

1. The directive. Walk the root's `children_with_tokens()` from the start, stopping at the first token that is not `NEWLINE`, `WHITESPACE`, or `COMMENT`, and at the first node of any kind. For each `COMMENT` token seen, strip a `#:` prefix, split on whitespace, and take a directive named `toml-version`. Parse its value as `1.0` or `1.1`. The first `toml-version` directive found wins; anything unparseable is skipped and resolution continues.
2. The configured value, when it is not `Auto`.
3. Detection: `root.descendants().any(|n| n.kind() == INLINE_TABLE && n.children_with_tokens().any(|c| c.kind() == NEWLINE))`. Note `children_with_tokens`, not `descendants_with_tokens`: a newline inside an array nested in the table belongs to the array, not the table, and `format_collection` at `mod.rs:882` already draws that distinction the same way for inline tables.
4. `V1_0`.

Add to the `create_options!` block in `mod.rs`, with a doc comment stating what it does and that `Auto` detects:

```rust
/// The TOML version the formatter targets.
///
/// `Auto` targets TOML 1.0 unless the document already contains a
/// multi-line inline table, so formatting never introduces syntax a
/// TOML 1.0 parser rejects. A `#:toml-version` directive in the
/// document outranks this value.
pub toml_version: TomlVersion,
```

and to the `Default for Options` impl: `toml_version: TomlVersion::Auto,`.

**Verify:** `cargo test -p taplo --all-features` → PASS, including the new `version::` tests. `cargo check -p taplo --all-features` → clean.

- [ ] **Step 3: Commit**

`feat(fmt): resolve a target toml version`

---

### Task 2: Gate the multi-line inline table

Make the resolved version decide what the formatter may emit. This is the task that fixes the defect.

**Files:**
- Modify: `crates/taplo/src/formatter/mod.rs` (`format_impl`)
- Test: `crates/taplo/src/tests/formatter.rs`
- Modify: `crates/taplo-cli/tests/multiline_inline_tables.rs`

**Interfaces:**
- Consumes: `resolve_version` from Task 1.
- `format_impl` clamps `options` before calling `format_root`. Every public entry point (`format`, `format_green`, `format_syntax`, `format_with_scopes`, `format_with_path_scopes`) reaches it, so none of them needs its own change.

- [ ] **Step 1: Write the failing tests**

In `crates/taplo/src/tests/formatter.rs`, using the existing test style in that file (build `Options`, call `format`, `assert_eq!` on the output):

1. `toml_1_0_leaves_a_wide_inline_table_on_one_line` — an entry wider than `column_width`, `Options::default()`. The inline table stays on one line. This fails today.
2. `toml_1_1_expands_a_wide_inline_table` — the same source with `toml_version: TomlVersion::V1_1`. It expands across lines.
3. `a_directive_expands_a_wide_inline_table` — the same source prefixed with `#:toml-version 1.1`, `Options::default()`. It expands, and the directive line survives in the output.
4. `toml_1_0_collapses_a_multiline_inline_table` — a short multi-line inline table with `toml_version: TomlVersion::V1_0` and `inline_table_auto_collapse: false`. It collapses anyway, because 1.0 forces collapse on.
5. `toml_1_0_keeps_a_commented_inline_table_multiline` — a multi-line inline table containing a comment, `toml_version: TomlVersion::V1_0`. It stays multi-line and the comment survives. This is spec R7: a comment outranks the version.
6. `an_already_multiline_document_keeps_expanding` — a source that already holds a multi-line inline table plus a second wide one, `Options::default()`. Detection resolves 1.1, so the wide one expands.

**Verify:** `cargo test -p taplo --all-features formatter::` → tests 1, 3, 4, 5 FAIL; 2 and 6 may already pass.

- [ ] **Step 2: Implement**

In `format_impl`, after the `ROOT` assertion:

```rust
let mut options = options;
if resolve_version(&node, options.toml_version) == ResolvedVersion::V1_0 {
    options.inline_table_expand = false;
    options.inline_table_auto_collapse = true;
}
```

with a comment saying why the clamp exists: a newline between the braces of an inline table is TOML 1.1 syntax, so targeting 1.0 means never producing one. Do not touch `inline_table_trailing_comma`; the guard at `mod.rs:995` already makes it unreachable when nothing renders multi-line.

Task 1's detection must run against the pre-clamp tree, which it does: `node` is the input document, not the output.

**Verify:** `cargo test -p taplo --all-features` → PASS.

- [ ] **Step 3: Update the CLI tests that assumed 1.1**

`crates/taplo-cli/tests/multiline_inline_tables.rs` exercises the multi-line inline table feature through the CLI. Every test whose *input* has no multi-line inline table but whose *expected output* does now resolves to 1.0 and fails. Run the suite, and for each failure add `"toml_version=1.1"` to that test's options array — the test is asserting 1.1 behavior and should say so. `expands_long_inline_tables` and `preserves_single_line_tables_when_expansion_is_disabled` are the expected candidates; run the suite rather than trusting that list.

Do not change any expected output string to match new behavior. If a test fails for a reason other than the version default, stop and report it: that is a real regression, not a test that needs updating.

**Verify:** `cargo test -p taplo-cli --test multiline_inline_tables` → PASS.

- [ ] **Step 4: Commit**

`feat(fmt): gate multiline inline tables on toml version`

---

### Task 3: Assert the invariant over the corpus

One hand-written case per behavior is not the requirement. Spec R6 asks that output parse as what it claims to be, across the whole formatter corpus.

**Files:**
- Test: `crates/taplo/src/tests/formatter.rs` (or a new `crates/taplo/src/tests/version_invariant.rs` wired into `crates/taplo/src/tests/mod.rs`)

**Interfaces:**
- Consumes: the option surface from Tasks 1 and 2.

- [ ] **Step 1: Write the test**

A single test that walks every `.toml` fixture the crate already has under `crates/taplo/test-data` (check the path with `fd -e toml`; use whatever directory the existing generated tests read) plus the inline sources in `crates/taplo/src/tests/formatter.rs`, and for each one, under both `TomlVersion::V1_0` and `TomlVersion::V1_1`:

1. Format it.
2. Re-parse the output with `crate::parser::parse`. Assert `errors.is_empty()`.
3. Under `V1_0` only: assert no `INLINE_TABLE` node in the re-parsed tree has a `NEWLINE` among its `children_with_tokens()`, *unless* that table contains a `COMMENT` — the R7 carve-out. The failure message names the fixture and the offending snippet.

Skip fixtures that fail to parse as input; the corpus includes deliberately invalid files (`tests::generated::invalid::*`), and formatting those is out of scope.

Assert the corpus is non-empty, so a wrong path silently testing nothing fails loudly.

**Verify:** `cargo test -p taplo --all-features` → PASS. Then confirm the test has teeth: temporarily revert Task 2's clamp, watch this test fail, and restore it. Report both outcomes.

- [ ] **Step 2: Commit**

`test(fmt): assert output matches its toml version`

---

### Task 4: Surface the option

The option exists in the Rust type but not yet anywhere a user can set it outside `.taplo.toml`.

**Files:**
- Modify: `editors/vscode/package.json`
- Modify: `js/core/src/formatter.ts`
- Modify: `site/site/configuration/formatter-options.md`

**Interfaces:**
- Consumes: the option name `toml_version` (snake_case) and `tomlVersion` (camelCase), and the values `auto`, `1.0`, `1.1`.

- [ ] **Step 1: Implement**

Follow the shape of the three inline-table options added in `b404d73`, which touched these same three files. In `package.json` the setting is an enum with the three values and a default of `auto`; in `formatter.ts` it is a union of the three string literals; in `formatter-options.md` it is a row matching the surrounding style, documenting the resolution order and the `#:toml-version` directive.

Document the directive in `site/site/configuration/file.md` too if that file is where document-level directives are described — check whether `#:schema` is documented there and match it.

**Verify:** `rg -n "tomlVersion|toml_version" /home/lev/Git/lev/taplo-wt/toml-version-gating/editors /home/lev/Git/lev/taplo-wt/toml-version-gating/js /home/lev/Git/lev/taplo-wt/toml-version-gating/site` shows all three surfaces. The VS Code `package.json` must stay valid JSON: `python3 -m json.tool` on it.

- [ ] **Step 2: Commit**

`docs(fmt): document the toml version option`
