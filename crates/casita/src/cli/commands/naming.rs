//! Stable machine-generated names for CLI-created roots.

use std::path::Path;

/// Canonicalize a local source, falling back to the spelling as given for
/// values such as remote URLs.
pub(super) fn canonical_source(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Compose a hierarchical root name from `prefix` and a source string.
pub(super) fn nested_name(prefix: &str, source: &str) -> String {
    let mut name = String::from(prefix);
    for segment in source.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            continue;
        }
        name.push('/');
        name.push_str(segment);
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_names_drop_empty_and_traversal_segments() {
        assert_eq!(
            nested_name("auto", "/home//./user/../project"),
            "auto/home/user/project"
        );
        assert_eq!(
            nested_name("auto/checkout", "/tmp/result"),
            "auto/checkout/tmp/result"
        );
    }
}
