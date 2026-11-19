use std::time::Duration;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_refresh_becomes_due_after_seven_days() {
        let interval = Duration::from_secs(7 * 24 * 60 * 60);
        assert!(!is_refresh_due(1_000, 1_000 + interval.as_secs() - 1, interval));
        assert!(is_refresh_due(1_000, 1_000 + interval.as_secs(), interval));
        assert!(is_refresh_due(1_000, 1_000 + interval.as_secs() + 1, interval));
    }
}
