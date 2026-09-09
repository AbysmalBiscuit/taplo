use std::{fmt, str::FromStr};

use crate::syntax::{SyntaxKind::*, SyntaxNode};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "schema")]
use schemars::JsonSchema;

/// The TOML version the formatter targets.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum TomlVersion {
    /// Target TOML 1.0 unless the document already contains syntax that
    /// requires TOML 1.1.
    #[default]
    Auto,
    #[cfg_attr(feature = "serde", serde(rename = "1.0"))]
    V1_0,
    #[cfg_attr(feature = "serde", serde(rename = "1.1"))]
    V1_1,
}

impl FromStr for TomlVersion {
    type Err = InvalidTomlVersion;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(TomlVersion::Auto),
            "1.0" => Ok(TomlVersion::V1_0),
            "1.1" => Ok(TomlVersion::V1_1),
            _ => Err(InvalidTomlVersion(s.into())),
        }
    }
}

impl fmt::Display for TomlVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TomlVersion::Auto => "auto",
            TomlVersion::V1_0 => "1.0",
            TomlVersion::V1_1 => "1.1",
        })
    }
}

/// The value of a `#:toml-version` directive, or an option, was not one of
/// `"auto"`, `"1.0"`, or `"1.1"`.
#[derive(Debug)]
pub struct InvalidTomlVersion(String);

impl fmt::Display for InvalidTomlVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, r#"invalid TOML version "{}""#, self.0)
    }
}

impl std::error::Error for InvalidTomlVersion {}

/// The TOML version a document was formatted against, with `Auto` already
/// resolved to a concrete version.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ResolvedVersion {
    V1_0,
    V1_1,
}

/// Resolves the TOML version to format against.
///
/// A `#:toml-version` directive at the top of the document outranks
/// `configured`. Absent a directive, `configured` is used unless it is
/// `Auto`, in which case the document is scanned for a multi-line inline
/// table: a syntax construct TOML 1.0 does not allow.
pub(crate) fn resolve_version(root: &SyntaxNode, configured: TomlVersion) -> ResolvedVersion {
    if let Some(version) = directive_version(root) {
        return version;
    }

    match configured {
        TomlVersion::V1_0 => ResolvedVersion::V1_0,
        TomlVersion::V1_1 => ResolvedVersion::V1_1,
        TomlVersion::Auto => detect_version(root),
    }
}

fn directive_version(root: &SyntaxNode) -> Option<ResolvedVersion> {
    for element in root.children_with_tokens() {
        let token = match &element {
            rowan::NodeOrToken::Token(t) => t,
            rowan::NodeOrToken::Node(_) => break,
        };

        match token.kind() {
            NEWLINE | WHITESPACE => continue,
            COMMENT => {
                if let Some(version) = parse_directive(token.text()) {
                    return Some(version);
                }
            }
            _ => break,
        }
    }

    None
}

fn parse_directive(text: &str) -> Option<ResolvedVersion> {
    let directive_content = text.strip_prefix("#:")?;
    let mut parts = directive_content.split_whitespace();

    if parts.next()? != "toml-version" {
        return None;
    }

    match parts.next()? {
        "1.0" => Some(ResolvedVersion::V1_0),
        "1.1" => Some(ResolvedVersion::V1_1),
        _ => None,
    }
}

fn detect_version(root: &SyntaxNode) -> ResolvedVersion {
    let has_multiline_inline_table = root
        .descendants()
        .any(|n| n.kind() == INLINE_TABLE && n.children_with_tokens().any(|c| c.kind() == NEWLINE));

    if has_multiline_inline_table {
        ResolvedVersion::V1_1
    } else {
        ResolvedVersion::V1_0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(src: &str, configured: TomlVersion) -> ResolvedVersion {
        let root = crate::parser::parse(src).into_syntax();
        resolve_version(&root, configured)
    }

    #[test]
    fn detects_compact_inline_table_as_v1_0() {
        assert_eq!(
            resolved("a = { x = 1 }", TomlVersion::Auto),
            ResolvedVersion::V1_0
        );
    }

    #[test]
    fn detects_multiline_inline_table_as_v1_1() {
        assert_eq!(
            resolved("a = {\n  x = 1,\n}", TomlVersion::Auto),
            ResolvedVersion::V1_1
        );
    }

    #[test]
    fn configured_v1_1_overrides_compact_source() {
        assert_eq!(
            resolved("a = { x = 1 }", TomlVersion::V1_1),
            ResolvedVersion::V1_1
        );
    }

    #[test]
    fn configured_v1_0_overrides_multiline_source() {
        assert_eq!(
            resolved("a = {\n  x = 1,\n}", TomlVersion::V1_0),
            ResolvedVersion::V1_0
        );
    }

    #[test]
    fn directive_overrides_auto_detection() {
        assert_eq!(
            resolved("#:toml-version 1.1\na = { x = 1 }", TomlVersion::Auto),
            ResolvedVersion::V1_1
        );
    }

    #[test]
    fn directive_v1_0_overrides_auto_detection() {
        assert_eq!(
            resolved("#:toml-version 1.0\na = {\n  x = 1,\n}", TomlVersion::Auto),
            ResolvedVersion::V1_0
        );
    }

    #[test]
    fn directive_outranks_configured_value() {
        assert_eq!(
            resolved("#:toml-version 1.0\na = { x = 1 }", TomlVersion::V1_1),
            ResolvedVersion::V1_0
        );
    }

    #[test]
    fn unparseable_directive_value_is_inert() {
        assert_eq!(
            resolved("#:toml-version 2.0\na = { x = 1 }", TomlVersion::Auto),
            ResolvedVersion::V1_0
        );
    }

    #[test]
    fn unparseable_directive_value_falls_through_to_detection() {
        assert_eq!(
            resolved(
                "#:toml-version banana\na = {\n  x = 1,\n}",
                TomlVersion::Auto
            ),
            ResolvedVersion::V1_1
        );
    }

    #[test]
    fn directive_not_at_top_is_ignored() {
        assert_eq!(
            resolved(
                "a = 1\n#:toml-version 1.1\nb = { x = 1 }",
                TomlVersion::Auto
            ),
            ResolvedVersion::V1_0
        );
    }

    #[test]
    fn plain_comment_is_not_a_directive() {
        assert_eq!(
            resolved("# toml-version 1.1\na = { x = 1 }", TomlVersion::Auto),
            ResolvedVersion::V1_0
        );
    }

    #[test]
    fn detection_reaches_nested_inline_table() {
        assert_eq!(
            resolved("a = [{ x = 1 }, {\n  y = 2,\n}]", TomlVersion::Auto),
            ResolvedVersion::V1_1
        );
    }

    #[test]
    fn from_str_parses_known_values() {
        assert_eq!("1.0".parse::<TomlVersion>().unwrap(), TomlVersion::V1_0);
        assert_eq!("1.1".parse::<TomlVersion>().unwrap(), TomlVersion::V1_1);
        assert_eq!("auto".parse::<TomlVersion>().unwrap(), TomlVersion::Auto);
    }

    #[test]
    fn from_str_rejects_unknown_values() {
        assert!("2.0".parse::<TomlVersion>().is_err());
        assert!("".parse::<TomlVersion>().is_err());
    }
}
