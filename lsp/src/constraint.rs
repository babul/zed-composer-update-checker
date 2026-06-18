//! Pragmatic Composer version-constraint handling.
//!
//! A full Composer resolver is out of scope. A dependency is "outdated" when a
//! newer stable release exists than the version pinned in its constraint — even
//! if the constraint would already permit that release (npm-update-checker
//! style). We extract the highest concrete version referenced (the *anchor*)
//! and compare it against the latest stable.

use semver::Version;

/// Normalize a raw version string (e.g. `v8.4`, `8.4.1.0`, `1.2.*`) into a
/// 3-segment [`Version`]. Wildcard segments (`*`, `x`) become `0`. Returns
/// `None` when the leading segment is not numeric.
pub fn normalize_version(raw: &str) -> Option<Version> {
    let raw = raw.trim().trim_start_matches(['v', 'V']);
    if raw.is_empty() {
        return None;
    }

    // Drop any stability/build suffix (`-RC1`, `-dev`, `+meta`).
    let core = raw.split(['-', '+']).next().unwrap_or(raw);

    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    Some(Version::new(major, minor, patch))
}

/// Extract the anchor version from a full constraint: the highest concrete
/// version referenced across `||` (OR) alternatives and space/comma separated
/// ranges, after stripping operators (`^ ~ >= > <= < = !=`). Returns `None` for
/// constraints we deliberately skip (`*`, `dev-*`, branch aliases).
pub fn anchor_version(constraint: &str) -> Option<Version> {
    constraint
        .split("||")
        .flat_map(|alt| alt.split([' ', ',', '\t']))
        .filter_map(parse_token)
        .max()
}

/// Parse one constraint token (e.g. `^8.4`, `>=1.0`, `1.2.*`) into a concrete
/// [`Version`]. Returns `None` for wildcards-only, dev branches, and stability
/// flags that carry no comparable version.
fn parse_token(token: &str) -> Option<Version> {
    let token = token.split('@').next().unwrap_or(token).trim();
    if token.is_empty() || token.starts_with("dev-") || token.ends_with("-dev") {
        return None;
    }

    let version_part = token.trim_start_matches(['^', '~', '>', '<', '=', '!']);
    if version_part.is_empty() || version_part == "*" {
        return None;
    }
    normalize_version(version_part)
}

/// True when a newer stable release exists than the constraint's anchor.
/// Non-evaluable constraints (`*`, `dev-*`, branch aliases) are never outdated.
pub fn is_outdated(constraint: &str, latest: &Version) -> bool {
    match anchor_version(constraint) {
        Some(anchor) => latest > &anchor,
        None => false,
    }
}

/// Build a suggested replacement constraint, preserving the user's operator
/// style: `^`/`~` are kept, an exact pin stays exact, anything else defaults to
/// a caret range.
pub fn suggested_constraint(original: &str, latest_display: &str) -> String {
    let trimmed = original.trim();
    if trimmed.starts_with('^') {
        format!("^{latest_display}")
    } else if trimmed.starts_with('~') {
        format!("~{latest_display}")
    } else if is_exact_pin(trimmed) {
        latest_display.to_string()
    } else {
        format!("^{latest_display}")
    }
}

/// An exact pin is a bare version with no operator, range, or wildcard.
fn is_exact_pin(constraint: &str) -> bool {
    let c = constraint.trim_start_matches(['v', 'V']);
    !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        normalize_version(s).unwrap()
    }

    #[test]
    fn flags_newer_release_within_range() {
        // npm-style: a newer release than the pinned version is outdated even
        // when the constraint already permits it.
        assert!(is_outdated("^9.0", &v("9.0.2")));
        assert!(is_outdated("^3.7.2", &v("3.10.0")));
        assert!(is_outdated("~1.2", &v("1.9.0")));
        assert!(is_outdated("1.2.*", &v("1.2.5")));
        assert!(is_outdated(">=4.1.1", &v("4.3.0")));
    }

    #[test]
    fn flags_out_of_range_major() {
        assert!(is_outdated("^8.0", &v("9.0.2")));
        assert!(is_outdated("^10.0", &v("12.19.0")));
    }

    #[test]
    fn not_outdated_at_or_below_anchor() {
        assert!(!is_outdated("^9.0", &v("9.0.0")));
        assert!(!is_outdated("1.2.3", &v("1.2.3")));
        // `<2.0` makes 2.0.0 the highest referenced version.
        assert!(!is_outdated(">=1.0 <2.0", &v("1.9.0")));
    }

    #[test]
    fn or_constraint_uses_highest_anchor() {
        assert!(!is_outdated("^7.0 || ^8.0", &v("8.0.0")));
        assert!(is_outdated("^7.0 || ^8.0", &v("8.2.0")));
        assert!(is_outdated("^7.0 || ^8.0", &v("9.0.0")));
    }

    #[test]
    fn handles_v_prefixed_constraint_tokens() {
        // e.g. `^v4.1.4` (the `v` lives inside the constraint).
        assert!(is_outdated("^v4.1.4", &v("4.7.3")));
        assert!(!is_outdated("^v4.7.3", &v("4.7.3")));
    }

    #[test]
    fn skipped_constraints_are_never_outdated() {
        assert!(!is_outdated("*", &v("99.0.0")));
        assert!(!is_outdated("dev-main", &v("99.0.0")));
        assert!(!is_outdated("1.x-dev", &v("99.0.0")));
        assert_eq!(anchor_version("*"), None);
        assert_eq!(anchor_version("dev-main"), None);
    }

    #[test]
    fn exact_pin_is_outdated_and_kept_exact() {
        assert!(is_outdated("1.2.3", &v("1.2.4")));
        assert_eq!(suggested_constraint("1.2.3", "1.2.4"), "1.2.4");
    }

    #[test]
    fn suggested_constraint_preserves_operator() {
        assert_eq!(suggested_constraint("^10.0", "12.19.0"), "^12.19.0");
        assert_eq!(suggested_constraint("~1.2", "2.0.0"), "~2.0.0");
        assert_eq!(suggested_constraint(">=1.0 <2.0", "3.0.0"), "^3.0.0");
    }

    #[test]
    fn normalizes_v_prefix_and_four_segments() {
        assert_eq!(v("v8.4"), Version::new(8, 4, 0));
        assert_eq!(v("8.4.1.0"), Version::new(8, 4, 1));
        assert_eq!(normalize_version("ext-json"), None);
    }
}
