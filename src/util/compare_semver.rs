use std::cmp::Ordering;

// N=3, much faster than BTreeMap
fn get_precedence(a: &str) -> u8 {
    match a {
        "" => 5,
        "rc" => 4,
        "beta" => 3,
        "alpha" => 2,
        _ => 1,
    }
}

fn compare_precedence(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }

    let (a_major, a_numeric) = a.split_once('.').unwrap_or((a, ""));
    let (b_major, b_numeric) = b.split_once('.').unwrap_or((b, ""));

    let a_precedence = get_precedence(a_major.trim());
    let b_precedence = get_precedence(b_major.trim());

    a_precedence.cmp(&b_precedence).then(
        a_numeric
            .parse::<u8>()
            .unwrap_or(0)
            .cmp(&b_numeric.parse::<u8>().unwrap_or(0)),
    )
}

fn parse_version(a: &str) -> Vec<u8> {
    a.split('.').filter_map(|s| s.parse::<u8>().ok()).collect()
}

/// Compare two semver strings. This function assumes that no numerical parts
/// of the version are larger than u8::MAX (255).
pub fn compare_semver(a: &str, b: &str) -> Ordering {
    let (a, _) = a.split_once('+').unwrap_or((a, ""));
    let (b, _) = b.split_once('+').unwrap_or((b, ""));

    if a == b {
        return Ordering::Equal;
    }

    let (a_version, a_build) = a.split_once('-').unwrap_or((a, ""));
    let (b_version, b_build) = b.split_once('-').unwrap_or((b, ""));

    let a_version: Vec<u8> = parse_version(a_version);
    let b_version: Vec<u8> = parse_version(b_version);

    for (a, b) in a_version.iter().zip(b_version.iter()) {
        match a.cmp(b) {
            Ordering::Equal => continue,
            x => return x,
        }
    }

    a_build
        .is_empty()
        .cmp(&b_build.is_empty())
        .then(compare_precedence(a_build, b_build))
}

#[cfg(test)]
mod tests {
    use super::compare_semver;
    use std::cmp::Ordering;

    #[test]
    fn test_equal_versions() {
        assert_eq!(compare_semver("1.0.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_semver("2.3.4", "2.3.4"), Ordering::Equal);
    }

    #[test]
    fn test_major_version_comparison() {
        assert_eq!(compare_semver("2.0.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare_semver("1.0.0", "2.0.0"), Ordering::Less);
    }

    #[test]
    fn test_minor_version_comparison() {
        assert_eq!(compare_semver("1.2.0", "1.1.9"), Ordering::Greater);
        assert_eq!(compare_semver("1.1.0", "1.2.0"), Ordering::Less);
    }

    #[test]
    fn test_patch_version_comparison() {
        assert_eq!(compare_semver("1.0.2", "1.0.1"), Ordering::Greater);
        assert_eq!(compare_semver("1.0.1", "1.0.2"), Ordering::Less);
    }

    #[test]
    fn test_pre_release_versions() {
        assert_eq!(compare_semver("1.0.0-alpha", "1.0.0"), Ordering::Less);
        assert_eq!(
            compare_semver("1.0.0-beta", "1.0.0-alpha"),
            Ordering::Greater
        );
        assert_eq!(
            compare_semver("1.0.0-alpha.1", "1.0.0-alpha"),
            Ordering::Greater
        );
        assert_eq!(
            compare_semver("4.2.1-beta.2", "4.2.1-beta.3"),
            Ordering::Less
        );
        assert_eq!(compare_semver("5.0.0-rc.1", "5.0.0"), Ordering::Less);
        assert_eq!(
            compare_semver("7.0.0-alpha.5", "7.0.0-alpha.4"),
            Ordering::Greater
        );
        assert_eq!(
            compare_semver("8.2.0-beta.10", "8.2.0-beta.2"),
            Ordering::Greater
        );
        assert_eq!(
            compare_semver("12.0.0-rc.3", "12.0.0-rc.2"),
            Ordering::Greater
        );
        assert_eq!(
            compare_semver("14.5.6-beta.4", "14.5.6-beta.3"),
            Ordering::Greater
        );
        assert_eq!(
            compare_semver("18.2.3-alpha.1", "18.2.3-alpha.2"),
            Ordering::Less
        );
        assert_eq!(
            compare_semver("22.1.0-rc.5", "22.1.0-rc.4"),
            Ordering::Greater
        );
        assert_eq!(
            compare_semver("27.9.0-alpha.9", "27.9.0-alpha.8"),
            Ordering::Greater
        );
        assert_eq!(compare_semver("29.1.8-rc.8", "29.1.8-rc.9"), Ordering::Less);
    }

    #[test]
    fn test_numeric_pre_release_alpha() {
        assert_eq!(
            compare_semver("1.0.0-alpha", "1.0.0-alpha.1"),
            Ordering::Less
        );
        assert_eq!(
            compare_semver("1.0.0-beta", "1.0.0-alpha.1"),
            Ordering::Greater
        );
        assert_eq!(compare_semver("1.0-alpha", "1.0.0-alpha.1"), Ordering::Less);
    }

    #[test]
    fn test_build_metadata_ignored() {
        assert_eq!(
            compare_semver("1.0.0+20130313144700", "1.0.0"),
            Ordering::Equal
        );
        assert_eq!(
            compare_semver("1.0.0-beta+exp.sha.5114f85", "1.0.0-beta"),
            Ordering::Equal
        );
        assert_eq!(compare_semver("6.1.3+build.456", "6.1.3"), Ordering::Equal);
        assert_eq!(compare_semver("16.3.1+exp.data", "16.3.1"), Ordering::Equal);
        assert_eq!(
            compare_semver("20.10.5-beta.6", "20.10.5-beta.6+hotfix.1001"),
            Ordering::Equal
        );
    }

    #[test]
    fn test_alternating_build_metadata() {
        assert_eq!(compare_semver("11.4.3", "11.4.3+meta.789"), Ordering::Equal);
        assert_eq!(
            compare_semver("15.0.0+final.release", "15.0.0"),
            Ordering::Equal
        );
        assert_eq!(
            compare_semver("19.0.0", "19.0.0+build.100"),
            Ordering::Equal
        );
        assert_eq!(
            compare_semver("23.7.9+commit.abc", "23.7.9"),
            Ordering::Equal
        );
        assert_eq!(
            compare_semver("28.0.0+revision.1", "28.0.0"),
            Ordering::Equal
        );
    }

    #[test]
    fn test_build_metadata_vs_regular() {
        assert_eq!(compare_semver("5.0.0+build.1", "4.9.9"), Ordering::Greater);
        assert_eq!(compare_semver("7.1.0+build.123", "7.1.1"), Ordering::Less);
        assert_eq!(
            compare_semver("9.3.2+xyz", "9.3.2-alpha"),
            Ordering::Greater
        );
        assert_eq!(compare_semver("12.4.0+meta.1", "12.5.0"), Ordering::Less);
        assert_eq!(
            compare_semver("14.2.1+patch.9", "14.2.1-rc.1"),
            Ordering::Greater
        );
    }

    #[test]
    fn test_precedence_ordering() {
        assert_eq!(
            compare_semver("1.0.0-alpha", "1.0.0-alpha.1"),
            Ordering::Less
        );
        assert_eq!(compare_semver("1.0.0-beta", "1.0.0-beta.2"), Ordering::Less);
        assert_eq!(
            compare_semver("1.0.0-beta.2", "1.0.0-beta.11"),
            Ordering::Less
        );
        assert_eq!(
            compare_semver("1.0.0-beta.11", "1.0.0-rc.1"),
            Ordering::Less
        );
        assert_eq!(compare_semver("1.0.0-rc.1", "1.0.0"), Ordering::Less);
    }
}
