//! Shared XML serialization for persisted state and machine-readable output.

use quick_xml::se::{SeError, Serializer};
use serde::Serialize;

/// Number of spaces used for each XML indentation level.
const INDENT_WIDTH: usize = 2;

/// Serializes `value` as indented XML terminated by one newline.
pub fn to_string<T: Serialize>(value: &T) -> Result<String, SeError> {
    let mut output = String::new();
    let mut serializer = Serializer::new(&mut output);
    serializer.indent(' ', INDENT_WIDTH);
    value.serialize(serializer)?;
    output.push('\n');
    Ok(output)
}

/// Returns whether a nested XML document is still stored on one line.
pub(crate) fn is_compact_nested(value: &str) -> bool {
    let value = value.trim();
    !value.contains('\n') && value.contains("><") && !value.ends_with("/>")
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::*;

    /// Nested document used to verify the shared serializer contract.
    #[derive(Serialize)]
    struct Document {
        /// Nested value serialized on its own indented line.
        child: Child,
    }

    /// Leaf value used by [`Document`].
    #[derive(Serialize)]
    struct Child {
        /// Text stored inside the leaf element.
        value: String,
    }

    #[test]
    /// Verifies indentation depth and trailing-newline behavior.
    fn serializes_nested_documents_with_two_spaces_and_a_newline() {
        let output = to_string(&Document {
            child: Child {
                value: "ready".to_string(),
            },
        })
        .unwrap();

        assert_eq!(
            output,
            "<Document>\n  <child>\n    <value>ready</value>\n  </child>\n</Document>\n"
        );
    }

    #[test]
    /// Distinguishes compact nested documents from empty root elements.
    fn detects_only_compact_nested_documents() {
        assert!(is_compact_nested("<root><child>value</child></root>"));
        assert!(!is_compact_nested(
            "<root>\n  <child>value</child>\n</root>\n"
        ));
        assert!(!is_compact_nested("<root/>\n"));
    }
}
