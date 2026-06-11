use crate::error::Error;
use semver::Version;

/// Compare two semver-like version strings.
/// Returns -1 if a < b, 0 if a == b, 1 if a > b.
/// Pre-release versions have lower precedence than the associated normal
/// version (SemVer 2.0 section 11): 1.0.0-alpha < 1.0.0.
pub fn compare_versions(a: &str, b: &str) -> i32 {
    let va = parse_version(a);
    let vb = parse_version(b);
    if va < vb {
        -1
    } else if va > vb {
        1
    } else {
        0
    }
}

/// Returns true if `candidate` is strictly newer than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    compare_versions(candidate, current) > 0
}

/// Validate that a version string is valid semver.
/// Accepts optional "v" prefix and relaxed 1-2 segment versions like "1.0" or "1".
pub fn validate_version(v: &str) -> Result<(), Error> {
    let v = v.trim();
    if v.is_empty() {
        return Err(Error::InvalidVersion("empty version".into()));
    }
    let bare = v.strip_prefix('v').unwrap_or(v);
    // Try full semver first
    if Version::parse(bare).is_ok() {
        return Ok(());
    }
    // Fallback: allow 1-3 numeric segments
    let parts: Vec<&str> = bare.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return Err(Error::InvalidVersion(format!(
            "{}: expected 1-3 numeric segments",
            v
        )));
    }
    for p in &parts {
        if p.is_empty() {
            return Err(Error::InvalidVersion(format!("{}: empty segment", v)));
        }
        for c in p.chars() {
            if !c.is_ascii_digit() {
                return Err(Error::InvalidVersion(format!(
                    "{}: non-numeric segment '{}'",
                    v, p
                )));
            }
        }
    }
    Ok(())
}

fn parse_version(s: &str) -> Version {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    // Strip build metadata for comparison (SemVer 2.0 §10: build metadata ignored for precedence)
    let s = s.split('+').next().unwrap_or(s);
    if let Ok(v) = Version::parse(s) {
        return v;
    }
    // Fallback for non-standard versions like "1.0" or "1"
    let (main_part, pre) = match s.find('-') {
        Some(idx) => (&s[..idx], Some(&s[idx + 1..])),
        None => (s, None),
    };
    let parts: Vec<&str> = main_part.split('.').collect();
    let major = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut v = Version::new(major, minor, patch);
    if let Some(pre) = pre {
        v.pre = semver::Prerelease::new(pre).unwrap_or_default();
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_versions() {
        let cases = [
            ("1.0.0", "1.0.0", 0),
            ("1.1.0", "1.0.0", 1),
            ("1.0.0", "1.1.0", -1),
            ("2.0.0", "1.9.9", 1),
            ("v1.0.0", "1.0.0", 0),
            ("v1.2.3", "v1.2.4", -1),
            ("1.0", "1.0.0", 0),
            ("1.0.0+build", "1.0.0", 0),
            ("0.9.9", "1.0.0", -1),
            ("10.0.0", "9.0.0", 1),
            // Pre-release: lower precedence than normal
            ("1.0.0-alpha", "1.0.0", -1),
            ("1.0.0", "1.0.0-alpha", 1),
            ("1.0.0-alpha", "1.0.0-alpha", 0),
            // Pre-release: numeric < alphanumeric
            ("1.0.0-1", "1.0.0-alpha", -1),
            // Pre-release: numeric comparison
            ("1.0.0-2", "1.0.0-10", -1),
            // Pre-release: lexicographic
            ("1.0.0-alpha", "1.0.0-beta", -1),
            // Pre-release: dot-separated identifiers
            ("1.0.0-alpha.1", "1.0.0-alpha.2", -1),
            // Shorter pre-release has lower precedence
            ("1.0.0-alpha", "1.0.0-alpha.1", -1),
        ];
        for (a, b, want) in cases {
            let got = compare_versions(a, b);
            assert_eq!(
                got, want,
                "compare_versions({:?}, {:?}) = {}, want {}",
                a, b, got, want
            );
        }
    }

    #[test]
    fn test_is_newer() {
        let cases = [
            ("1.1.0", "1.0.0", true),
            ("1.0.0", "1.0.0", false),
            ("0.9.0", "1.0.0", false),
            ("2.0.0", "1.9.9", true),
            ("v1.1.0", "v1.0.0", true),
            ("1.0.0", "1.0.0-beta", true),
            ("1.0.0-beta", "1.0.0", false),
        ];
        for (candidate, current, want) in cases {
            let got = is_newer(candidate, current);
            assert_eq!(
                got, want,
                "is_newer({:?}, {:?}) = {}, want {}",
                candidate, current, got, want
            );
        }
    }

    #[test]
    fn test_validate_version() {
        for v in &[
            "1.0.0",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0+build",
            "1.0",
            "1",
        ] {
            validate_version(v)
                .unwrap_or_else(|e| panic!("validate_version({:?}) should be valid: {}", v, e));
        }
        for v in &["", "1..0", "1.0.0a", "1.a.0"] {
            assert!(
                validate_version(v).is_err(),
                "validate_version({:?}) should fail",
                v
            );
        }
    }
}
