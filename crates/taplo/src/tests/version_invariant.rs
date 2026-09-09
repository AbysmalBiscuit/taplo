//! Asserts spec R6 over the formatter fixture corpus: formatting any fixture
//! under either target TOML version produces output that re-parses cleanly,
//! and output targeting TOML 1.0 never contains a multi-line inline table
//! (the one construct the formatter can introduce that 1.0 forbids), except
//! where R7 exempts an inline table holding a comment.

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

#[test]
fn formatted_output_matches_its_toml_version() {
    let mut fixtures = Vec::new();
    collect_fixtures(&corpus_root(), &mut fixtures);
    assert!(
        !fixtures.is_empty(),
        "found no .toml fixtures under {} - check the corpus path",
        corpus_root().display()
    );

    for fixture in &fixtures {
        let src = std::fs::read_to_string(fixture)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", fixture.display()));

        if !crate::parser::parse(&src).errors.is_empty() {
            // Not one of the fixtures this invariant covers; skip rather
            // than assert on input the corpus never promised was valid.
            continue;
        }

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
        }
    }
}
