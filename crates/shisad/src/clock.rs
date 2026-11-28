use std::time::{SystemTime, UNIX_EPOCH};

/// Unix milliseconds for the daemon's D-Bus activity and diagnostics fields.
pub(crate) fn now_ms() -> i64 {
    unix_millis(SystemTime::now())
}

fn unix_millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn unix_milliseconds_drop_submillisecond_precision_and_clamp_before_epoch() {
        assert_eq!(
            unix_millis(UNIX_EPOCH + Duration::from_micros(1_234_567)),
            1_234
        );
        assert_eq!(unix_millis(UNIX_EPOCH), 0);
        assert_eq!(unix_millis(UNIX_EPOCH - Duration::from_secs(1)), 0);
    }
}
