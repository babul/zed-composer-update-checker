//! Parse `composer.json` text into the set of real package dependencies, each
//! tagged with the source range of its constraint value string (used for both
//! diagnostics and code-action edits).

use serde_json::Value;
use tower_lsp::lsp_types::{Position, Range};

/// A single `vendor/package` dependency drawn from `require` / `require-dev`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub constraint: String,
    /// Range of the constraint *value* (the text between the quotes).
    pub value_range: Range,
    pub dev: bool,
}

/// Extract every comparable dependency from a `composer.json` document.
///
/// Platform requirements (`php`, `ext-*`, `lib-*`, `composer-*-api`) have no
/// `/` and are skipped. Non-string values are ignored.
pub fn parse_dependencies(text: &str) -> Vec<Dependency> {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };

    let mut deps = Vec::new();
    for (section, dev) in [("require", false), ("require-dev", true)] {
        let Some(Value::Object(entries)) = root.get(section) else {
            continue;
        };
        for (name, value) in entries {
            let Some(constraint) = value.as_str() else {
                continue;
            };
            if !is_package_name(name) {
                continue;
            }
            if let Some(value_range) = find_value_range(text, name, constraint) {
                deps.push(Dependency {
                    name: name.clone(),
                    constraint: constraint.to_string(),
                    value_range,
                    dev,
                });
            }
        }
    }
    deps
}

/// A real Composer package is always `vendor/name`. This naturally excludes
/// `php`, `ext-*`, `lib-*`, `composer-plugin-api`, and `composer-runtime-api`.
fn is_package_name(key: &str) -> bool {
    key.contains('/')
}

/// Locate the range of the constraint value for `name` whose text equals
/// `constraint`. Matching on the value disambiguates a package that appears in
/// both `require` and `require-dev`.
fn find_value_range(text: &str, name: &str, constraint: &str) -> Option<Range> {
    let key = format!("\"{name}\"");
    let bytes = text.as_bytes();
    let mut search_from = 0;

    while let Some(rel) = text[search_from..].find(&key) {
        let key_start = search_from + rel;
        let mut i = key_start + key.len();

        i = skip_ascii_ws(bytes, i);
        if bytes.get(i) == Some(&b':') {
            i = skip_ascii_ws(bytes, i + 1);
            if bytes.get(i) == Some(&b'"') {
                let val_start = i + 1;
                if let Some(rel2) = text[val_start..].find('"') {
                    let val_end = val_start + rel2;
                    if &text[val_start..val_end] == constraint {
                        return Some(Range {
                            start: offset_to_position(text, val_start),
                            end: offset_to_position(text, val_end),
                        });
                    }
                }
            }
        }
        search_from = key_start + key.len();
    }
    None
}

fn skip_ascii_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Convert a byte offset into an LSP [`Position`] (UTF-16 column units).
fn offset_to_position(text: &str, offset: usize) -> Position {
    let mut line = 0u32;
    let mut character = 0u32;
    for (i, ch) in text.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }
    Position { line, character }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
    "name": "acme/app",
    "require": {
        "php": "^8.2",
        "ext-json": "*",
        "laravel/framework": "^10.0",
        "composer-runtime-api": "^2.0"
    },
    "require-dev": {
        "pestphp/pest": "^2.0"
    }
}"#;

    #[test]
    fn extracts_only_real_packages() {
        let deps = parse_dependencies(SAMPLE);
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["laravel/framework", "pestphp/pest"]);
    }

    #[test]
    fn excludes_php_ext_and_platform() {
        let deps = parse_dependencies(SAMPLE);
        assert!(deps.iter().all(|d| d.name != "php"));
        assert!(deps.iter().all(|d| !d.name.starts_with("ext-")));
        assert!(deps.iter().all(|d| !d.name.starts_with("composer-")));
    }

    #[test]
    fn tags_dev_section() {
        let deps = parse_dependencies(SAMPLE);
        let dev = deps.iter().find(|d| d.name == "pestphp/pest").unwrap();
        assert!(dev.dev);
        let prod = deps.iter().find(|d| d.name == "laravel/framework").unwrap();
        assert!(!prod.dev);
    }

    #[test]
    fn value_range_covers_constraint_text() {
        let deps = parse_dependencies(SAMPLE);
        let laravel = deps.iter().find(|d| d.name == "laravel/framework").unwrap();
        // Line index of `"laravel/framework": "^10.0"` (0-based) is 5.
        assert_eq!(laravel.value_range.start.line, 5);
        // The slice between the recorded columns is exactly the constraint.
        let line = SAMPLE.lines().nth(5).unwrap();
        let start = laravel.value_range.start.character as usize;
        let end = laravel.value_range.end.character as usize;
        assert_eq!(&line[start..end], "^10.0");
    }
}
