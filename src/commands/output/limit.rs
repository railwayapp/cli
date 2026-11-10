use colored::Colorize;

/// Upper bound the API accepts for `first` on list queries.
pub(in crate::commands) const MAX_LIST_LIMIT: i64 = 1000;

/// Clamp a user-supplied `--limit` into `1..=MAX_LIST_LIMIT`, warning on stderr
/// when it had to be adjusted.
pub(in crate::commands) fn clamp_list_limit(limit: i64) -> i64 {
    if limit > MAX_LIST_LIMIT {
        eprintln!(
            "{}",
            format!(
                "Warning: limit cannot exceed {MAX_LIST_LIMIT}, using {MAX_LIST_LIMIT} instead"
            )
            .yellow()
        );
        MAX_LIST_LIMIT
    } else if limit < 1 {
        eprintln!(
            "{}",
            "Warning: limit must be at least 1, using 1 instead".yellow()
        );
        1
    } else {
        limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_into_range() {
        assert_eq!(clamp_list_limit(0), 1);
        assert_eq!(clamp_list_limit(-5), 1);
        assert_eq!(clamp_list_limit(20), 20);
        assert_eq!(clamp_list_limit(1000), 1000);
        assert_eq!(clamp_list_limit(5000), 1000);
    }
}
