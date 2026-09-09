# Directives

The behaviour of Taplo can be further customized by comments in TOML files called `directives`.

All directive comments must follow the following pattern: `#:<name> <content>`.

A `header` directive means that it is at the beginning of the document and can only be preceded by other directives or comments.

## The `schema` Directive

It is possible to override the schema for a specific document by using the `schema` header directive. A relative file path or an URL can be provided.

Example:

```toml
#:schema ./foo-schema.json
foo = "bar"
```

::: tip

Relative paths are relative to the document file, if the file path is not known, Taplo will be unable to find the schema.

:::

::: warning

Multiple schema directives in the same document are not supported and the behaviour is undefined.

:::

## The `toml-version` Directive

The `toml-version` header directive declares the TOML version a document is written in, `1.0` or `1.1`, and outranks the [`toml_version` formatter option](./formatter-options.md). It determines whether the formatter may leave or produce a multi-line inline table, a TOML 1.1 construct that a TOML 1.0 parser rejects.

Example:

```toml
#:toml-version 1.1
values = { a = 1, b = 2 }
```

::: tip

An unrecognized value, such as `#:toml-version 2.0`, is ignored, and version resolution falls through to the configured option and then to detection, as described on the [Formatter Options](./formatter-options.md) page.

:::
