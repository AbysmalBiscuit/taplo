# TOML version gating for the formatter

**Status:** Accepted

## Problem

The formatter emits TOML 1.1 syntax into files nothing declared to be TOML 1.1, and the result is not a valid TOML 1.0 document. A file that parsed before formatting does not parse after.

TOML 1.0 forbids a newline anywhere between the braces of an inline table. Multi-line inline tables, and the trailing comma that comes with them, are a TOML 1.1 draft feature. The formatter has three options that produce them, all defaulting to on: `inline_table_expand`, `inline_table_auto_collapse`, and `inline_table_trailing_comma`.

Released `taplo` 0.10.0 already does this. Given a valid input:

```toml
[tasks.verify]
description = "Check formatting, lint, and run tests including documentation tests"
steps = [{ task = "fmt-check" }, { task = "lint" }, { task = "test" }, { task = "test-doc" }]
```

it produces:

```toml
steps = [
  {
    task = "fmt-check",
  },
```

Python's `tomllib`, a TOML 1.0 parser, accepts the input and rejects the output with `Invalid initial character for a key part`. The same holds for the current branch when `inline_table_expand` fires on an entry wider than `column_width`:

```toml
long_it = {
  name = "something",
  description = "a fairly long description here",
  enabled = true,
  retries = 5,
}
```

Both shapes are rejected regardless of entry count. `{ a = 1, }` and `{ a = 1, b = 2, }` are each rejected on their own; the trailing comma is never legal in a single-line inline table, and the newline is never legal in a 1.0 inline table at all.

The trailing-comma guard at `crates/taplo/src/formatter/mod.rs:995` is already correct:

```rust
let has_comma = node_index < node_count - 1 || (multiline && trailing_comma);
```

It reads `multiline` after the collapse at `mod.rs:894` has cleared it, so a table that renders on one line never gets a trailing comma. Probing the current branch's release build, the 0.10.0 build, and the debug build across all sixteen combinations of `inline_table_expand`, `inline_table_auto_collapse`, `inline_table_trailing_comma`, and `array_auto_collapse` produced no single-line table carrying a trailing comma. Feeding `{ task = "fmt-check", }` back in, the current branch repairs it.

So the defect is not the comma. It is that the formatter chooses a TOML version by accident, and the version it chooses by accident is one that had not shipped.

## What the formatter can introduce

The formatter rewrites layout, not lexical content. `format_key` at `mod.rs:796` takes `_options` and `_context` unused; strings pass through as written. Multi-line arrays and trailing commas inside them are valid TOML 1.0. Of the syntax that separates TOML 1.1 from TOML 1.0, the only construct this formatter can put into a file that did not have one is the multi-line inline table.

That bounds the work. Gating one construct gates every version-dependent thing the formatter can emit.

## Detection is one-directional

A file containing a multi-line inline table is provably TOML 1.1 or later. A file containing none is not provably TOML 1.0; it is compatible with both. Detection can therefore raise a version but never establish one, and most files carry no version-specific syntax at all.

What detection is good for is a floor: the formatter must not introduce syntax the input did not already use. Under that rule a valid TOML 1.0 file can never come out invalid, which is the defect above, fixed.

## Requirements

**R1. A version travels with the format call.** `Options` carries a `toml_version` field. It is part of the formatter's public option surface: settable from `.taplo.toml`, from the CLI's `--option` flag, from the VS Code settings, and from the JS bindings, the same way every other option is.

**R2. The default introduces nothing.** With no version configured and no directive, the formatter targets TOML 1.0 unless the document already contains a multi-line inline table, in which case it targets 1.1. A document with no multi-line inline table never gains one.

**R3. A file can declare its own version.** A `#:toml-version 1.1` comment declares the document's version and outranks any configured value. The directive mechanism exists: `crates/taplo/src/dom/mod.rs:337` parses any `#:name value` comment into a name/value pair, and `#:schema` is one consumer of it. Only a directive at the top of the document, before the first entry or table header, is honored.

**R4. An explicit version outranks detection.** A configured `toml_version` is obeyed whether it raises or lowers what detection would have chosen. Configuring 1.0 against a document that already holds multi-line inline tables collapses them rather than preserving them.

**R5. Targeting 1.0 suppresses the construct, not the option.** Under 1.0, `inline_table_expand` does not expand: an inline table too wide for `column_width` stays on one line. `inline_table_auto_collapse` is forced on, because collapsing moves the document toward 1.0, and it overrides a configured `false`. `inline_table_trailing_comma` becomes unreachable, since it fires only on a table that renders multi-line.

**R6. Output parses as what it claims to be.** Formatter output re-parses, and when the resolved version is 1.0 no inline table in the output contains a newline. This is asserted over the existing formatter corpus, not over one hand-written case.

**R7. A comment outranks the version.** An inline table containing a comment cannot be collapsed without dropping the comment, so under 1.0 it is left as found. The formatter never discards a comment to satisfy a version. This is the one exception to R6 and the invariant test states it.

**R8. An unparseable version is inert.** `#:toml-version 2.0`, `#:toml-version banana`, or a malformed configured value leaves resolution where it would have been without it. Formatting never fails because a version could not be read.

## Resolution order

Highest first. The first one that yields a version wins.

1. A `#:toml-version` directive at the top of the document.
2. A configured `toml_version`, from `.taplo.toml`, the CLI, the editor, or the JS bindings.
3. Detection: 1.1 if the document already contains a multi-line inline table.
4. TOML 1.0.

## Non-goals

- Gating any construct other than the multi-line inline table. The formatter cannot introduce the others.
- Parser-side version enforcement. Taplo's parser accepts 1.1 syntax and continues to; this is a formatter output contract, not a validation feature.
- Standardizing the directive. `#:toml-version` is taplo-specific exactly as `#:schema` is, and no other tool will honor it.
- Tracking the TOML 1.1 draft's other additions. 1.1 has not shipped and its feature list can still move.

## Open questions

None. The resolution order settles the precedence, and the construct list is closed by what the formatter is capable of emitting.
