use std::{
    io::Write,
    process::{Command, Stdio},
};

fn format(source: &str, options: &[&str]) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_taplo"));
    command.args(["format", "--no-auto-config", "-"]);
    command.env_remove("TAPLO_CONFIG");
    for option in options {
        command.args(["--option", option]);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn assert_format(source: &str, expected: &str, options: &[&str]) {
    let formatted = format(source, options);
    assert_eq!(formatted, expected);
    let parsed = taplo::parser::parse(&formatted);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let original = serde_json::to_value(taplo::parser::parse(source).into_dom()).unwrap();
    let result = serde_json::to_value(parsed.into_dom()).unwrap();
    assert_eq!(result, original);
    assert_eq!(format(&formatted, options), formatted);
}

#[test]
fn preserves_inline_table_comments() {
    assert_format(
        "dependency={\n# version\nversion=\"1\", # keep\nfeatures=[\"derive\"] # before comma\n,\n}\n",
        "dependency = {\n  # version\n  version = \"1\", # keep\n  features = [\"derive\"], # before comma\n}\n",
        &["align_comments=false"],
    );
}

#[test]
fn preserves_comments_on_both_sides_of_a_comma() {
    assert_format(
        "table={a=1 # before\n, # after\nb=2}\n",
        "table = {\n  a = 1, # before\n  # after\n  b = 2,\n}\n",
        &[],
    );
}

#[test]
fn expands_long_inline_tables() {
    assert_format(
        "dependency = { version = \"1\", optional = true }\n",
        "dependency = {\n  version = \"1\",\n  optional = true,\n}\n",
        &["column_width=40", "toml_version=1.1"],
    );
}

#[test]
fn collapses_short_inline_tables() {
    assert_format(
        "dependency = {\n  version = \"1\",\n  optional = true,\n}\n",
        "dependency = { version = \"1\", optional = true }\n",
        &[],
    );
}

#[test]
fn preserves_multiline_layout_when_collapse_is_disabled() {
    assert_format(
        "dependency={\nversion=\"1\",optional=true\n}\n",
        "dependency = {\n  version = \"1\",\n  optional = true,\n}\n",
        &["inline_table_auto_collapse=false"],
    );
}

#[test]
fn omits_optional_trailing_comma() {
    assert_format(
        "dependency={\nversion=\"1\",optional=true,\n}\n",
        "dependency = {\n  version = \"1\",\n  optional = true\n}\n",
        &[
            "inline_table_auto_collapse=false",
            "inline_table_trailing_comma=false",
        ],
    );
}

#[test]
fn preserves_comments_in_empty_inline_tables() {
    for source in ["empty={ # keep\n}\n", "empty={\n# keep\n}\n"] {
        let expected = if source.contains("{ #") {
            "empty = { # keep\n}\n"
        } else {
            "empty = {\n  # keep\n}\n"
        };
        assert_format(source, expected, &[]);
    }
    assert_format("empty={}\n", "empty = {}\n", &[]);
}

#[test]
fn sorts_entries_with_their_comments() {
    assert_format(
        "table={\nz=1, # z\na=2, # a\n\n# keep this group\ny=3,\nb=4,\n}\n",
        "table = {\n  a = 2, # a\n  z = 1, # z\n\n  # keep this group\n  b = 4,\n  y = 3,\n}\n",
        &["reorder_inline_tables=true"],
    );
}

#[test]
fn formats_nested_tables_and_arrays() {
    assert_format(
        "items=[\n{\nchild={\na=1,\nb=2,\n},\nvalues=[1,2],\n},\n]\n",
        "items = [\n  {\n    child = {\n      a = 1,\n      b = 2,\n    },\n    values = [1, 2],\n  },\n]\n",
        &[
            "inline_table_auto_collapse=false",
            "array_auto_collapse=false",
        ],
    );
}

#[test]
fn preserves_nested_comments() {
    assert_format(
        "table={child={value=1 # keep\n}}\n",
        "table = {\n  child = {\n    value = 1, # keep\n  },\n}\n",
        &[],
    );
}

#[test]
fn honors_crlf_and_indentation() {
    assert_format(
        "table={\na=1,b=2\n}\n",
        "table = {\r\n\ta = 1,\r\n\tb = 2,\r\n}\r\n",
        &[
            "inline_table_auto_collapse=false",
            "crlf=true",
            "indent_string=\t",
        ],
    );
}

#[test]
fn preserves_single_line_tables_when_expansion_is_disabled() {
    let source = "dependency = { version = \"1\", optional = true }\n";
    assert_format(
        source,
        source,
        &["column_width=20", "inline_table_expand=false"],
    );
}

#[test]
fn keeps_a_nested_comment_out_of_the_toml_version() {
    assert_format(
        "a = { b = [1, # keep\n2] }\n",
        "a = { b = [\n  1, # keep\n  2,\n] }\n",
        &[],
    );
}
