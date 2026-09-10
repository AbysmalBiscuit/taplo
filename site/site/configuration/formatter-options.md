# Formatter Options

This page contains a list of formatting options the formatter accepts.

::: warning

In some environments (e.g. in Visual Studio Code and JavaScript) the option keys are _camelCase_ to better fit the conventions. For example `align_entries` becomes `alignEntries`.

In some environments (e.g., Visual Studio Code), one needs to reload the extension to let the settings take effect.

:::

|        option         |                                                          description                                                           | default value  |
| :-------------------: | :----------------------------------------------------------------------------------------------------------------------------: | :------------: |
|     align_entries     |       Align entries vertically. Entries that have table headers, comments, or blank lines between them are not aligned.        |     false      |
|    align_comments     | Align consecutive comments after entries and items vertically. This applies to comments that are after entries or array items. |      true      |
| array_trailing_comma  |                                           Put trailing commas for multiline arrays.                                            |      true      |
|   array_auto_expand   |                   Automatically expand arrays to multiple lines when they exceed `column_width` characters.                    |      true      |
|  array_auto_collapse  |                                     Automatically collapse arrays if they fit in one line.                                     |      true      |
|    compact_arrays     |                                       Omit whitespace padding inside single-line arrays.                                       |      true      |
| compact_inline_tables |                                   Omit whitespace padding inside single-line inline tables.                                   |     false      |
|  inline_table_expand  |       Expand inline tables and their values when they exceed `column_width`. Requires `array_auto_expand`. Suppressed when the resolved `toml_version` is `1.0`, which is what a document holding no multi-line inline table resolves to by default.                    |      true      |
| inline_table_auto_collapse |                      Collapse multiline inline tables if they fit in one line and contain no comments.                      |      true      |
| inline_table_trailing_comma |                                    Put trailing commas for multiline inline tables.                                         |      true      |
|    compact_entries    |                                                  Omit whitespace around `=`.                                                   |     false      |
|     column_width      |                          Target maximum column width after which arrays are expanded into new lines.                           |       80       |
|     indent_tables     |                                            Indent subtables if they come in order.                                             |     false      |
|    indent_entries     |                                                  Indent entries under tables.                                                  |     false      |
|     indent_string     |                        Indentation to use, should be tabs or spaces but technically could be anything.                         | 2 spaces (" ") |
|   trailing_newline    |                                              Add trailing newline to the source.                                               |      true      |
|     reorder_keys      |                               Alphabetically reorder keys that are not separated by blank lines.                               |     false      |
|    reorder_arrays     |                           Alphabetically reorder array values that are not separated by blank lines.                           |     false      |
| reorder_inline_tables |                       Alphabetically reorder inline table entries within groups separated by comments or blank lines.                       |     false      |
|  allowed_blank_lines  |                                     The maximum amount of consecutive blank lines allowed.                                     |       2        |
|         crlf          |                                                     Use CRLF line endings.                                                     |     false      |
|      toml_version      |                    The TOML version the formatter targets: `auto`, `1.0`, or `1.1`. See below for what each targets.                    |     auto       |

Multiline inline tables use TOML 1.1 syntax. To keep their multiline layout even when the entries fit on one line, set:

```toml
[formatting]
inline_table_auto_collapse = false
```

This keeps nothing multiline when the document targets TOML 1.0, either through `toml_version = "1.0"` or a `#:toml-version 1.0` directive: collapsing is forced on at every scope so that the output parses as TOML 1.0.

Comments inside inline tables are preserved. Trailing comments move with their entries when `reorder_inline_tables` is enabled.

`toml_version` picks which TOML version the formatter's output must parse as. Targeting `1.0` suppresses the multi-line inline table: `inline_table_expand` no longer expands an entry that is too wide for `column_width`, and `inline_table_auto_collapse` is forced on, overriding a configured `false`, so no inline table is left multi-line. An inline table that lays out a comment of its own is the one exception, it is left multi-line regardless, because collapsing it would drop the comment. A comment nested inside one of its arrays does not stop it collapsing: the array writes that comment on its own line, which TOML 1.0 allows between an inline table's braces because it falls inside a value. Targeting `1.1` allows multi-line inline tables as normal. The version used to format a document is resolved in this order, highest priority first: a `#:toml-version` directive at the top of the document (see [Directives](./directives.md)), the configured `toml_version` when it is not `auto`, detection of an existing multi-line inline table in the document, and finally `1.0`. The default, `auto`, means the formatter never introduces a multi-line inline table into a document that did not already have one. A document has one version, so `toml_version` is resolved once per document and is ignored inside `[rule.formatting]`.
