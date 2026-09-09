//! Formatting any fixture in the corpus under either target TOML version
//! produces output that re-parses, and output targeting TOML 1.0 never
//! contains a multi-line inline table: the one construct the formatter can
//! introduce that TOML 1.0 forbids. An inline table holding a comment is the
//! exception, it stays multi-line because collapsing it would drop the
//! comment.

use std::path::{Path, PathBuf};

use crate::formatter::{self, Options, TomlVersion};
use crate::syntax::SyntaxKind;

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-data")
}

/// Collects every `.toml` fixture under `dir`, recursing into
/// subdirectories except one named `invalid` (deliberately unparseable
/// fixtures backing `tests::generated::invalid`, out of scope here).
fn collect_fixtures(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()));

    for entry in entries {
        let entry = entry.unwrap();
        let path = entry.path();

        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("invalid") {
                continue;
            }
            collect_fixtures(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            out.push(path);
        }
    }
}

/// Every `INLINE_TABLE` node in `node`'s tree whose immediate
/// `children_with_tokens()` contains a `NEWLINE`, together with its source
/// text. Matches `version::detect_version`'s own definition of
/// "multi-line": a newline that is a direct child of the table, not one
/// belonging to a nested array or table.
fn multiline_inline_tables(node: &crate::syntax::SyntaxNode) -> Vec<crate::syntax::SyntaxNode> {
    node.descendants()
        .filter(|n| n.kind() == SyntaxKind::INLINE_TABLE)
        .filter(|n| {
            n.children_with_tokens()
                .any(|c| c.kind() == SyntaxKind::NEWLINE)
        })
        .collect()
}

fn contains_comment(node: &crate::syntax::SyntaxNode) -> bool {
    node.children_with_tokens()
        .any(|c| c.kind() == SyntaxKind::COMMENT)
}

fn parses_as_toml_1_0(src: &str) -> Result<(), toml::de::Error> {
    toml::from_str::<toml::Value>(src).map(|_| ())
}

#[test]
fn formatted_output_matches_its_toml_version() {
    let mut fixtures = Vec::new();
    collect_fixtures(&corpus_root(), &mut fixtures);

    let mut checked_against_v1_0 = Vec::new();

    for fixture in &fixtures {
        let src = std::fs::read_to_string(fixture)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", fixture.display()));

        if !crate::parser::parse(&src).errors.is_empty() {
            // Not one of the fixtures this invariant covers; skip rather
            // than assert on input the corpus never promised was valid.
            continue;
        }

        // taplo's parser accepts more than TOML 1.0 on purpose, so a fixture
        // the 1.0 parser already rejects as input says nothing about whether
        // the formatter kept the output within 1.0.
        let input_is_toml_1_0 = parses_as_toml_1_0(&src).is_ok();

        for configured in [TomlVersion::V1_0, TomlVersion::V1_1] {
            let formatted = formatter::format(
                &src,
                Options {
                    toml_version: configured,
                    ..Default::default()
                },
            );

            let reparsed = crate::parser::parse(&formatted);
            assert!(
                reparsed.errors.is_empty(),
                "{} formatted for TomlVersion::{configured:?} does not re-parse: {:#?}\n---\n{formatted}",
                fixture.display(),
                reparsed.errors,
            );

            if configured != TomlVersion::V1_0 {
                continue;
            }

            let root = reparsed.into_syntax();
            for table in multiline_inline_tables(&root) {
                if contains_comment(&table) {
                    continue;
                }
                panic!(
                    "{} formatted for TomlVersion::V1_0 kept a multi-line \
                     inline table with no comment (invalid TOML 1.0): {}",
                    fixture.display(),
                    table.text(),
                );
            }

            if !input_is_toml_1_0 {
                continue;
            }

            if let Err(err) = parses_as_toml_1_0(&formatted) {
                panic!(
                    "{} formatted for TomlVersion::V1_0 is not valid TOML 1.0: {err}\n---\n{formatted}",
                    fixture.display(),
                );
            }

            checked_against_v1_0.push(fixture.clone());
        }
    }

    // The corpus only exercises the version clamp through fixtures that are
    // wide enough for the formatter to want to expand an inline table.
    // Without this one, the test passes with the clamp removed.
    let expanding_fixture = corpus_root().join("inline_table_expand.toml");
    assert!(
        checked_against_v1_0.contains(&expanding_fixture),
        "{} was not checked against TOML 1.0 - without it this test has no teeth",
        expanding_fixture.display(),
    );
}
