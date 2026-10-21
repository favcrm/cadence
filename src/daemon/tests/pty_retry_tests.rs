use super::*;

#[test]
fn retry_base_defaults_to_five_seconds() {
    assert_eq!(parse_pty_retry_base(None), Ok(Duration::from_secs(5)));
}

#[test]
fn retry_base_accepts_seconds_inside_the_bounds() {
    assert_eq!(parse_pty_retry_base(Some("1")), Ok(Duration::from_secs(1)));
    assert_eq!(
        parse_pty_retry_base(Some(" 2.5 ")),
        Ok(Duration::from_millis(2500))
    );
    assert_eq!(
        parse_pty_retry_base(Some("0.1")),
        Ok(Duration::from_millis(100))
    );
    assert_eq!(
        parse_pty_retry_base(Some("3600")),
        Ok(Duration::from_secs(3600))
    );
}

#[test]
fn retry_base_refuses_values_outside_the_bounds_or_not_numbers() {
    for raw in [
        "0", "-1", "0.05", "0.099", "3600.5", "1e9", "NaN", "nan", "inf", "-inf", "", "five", "5s",
    ] {
        let got = parse_pty_retry_base(Some(raw));
        assert!(got.is_err(), "{raw:?} must be refused, got {got:?}");
        assert!(
            got.unwrap_err().contains("CADENCE_PTY_RETRY_SECS"),
            "reason names the knob for {raw:?}"
        );
    }
}

#[test]
fn gate_backoff_doubles_to_a_six_times_cap() {
    let five = Duration::from_secs(5);
    let schedule: Vec<u64> = (0..6).map(|n| gate_backoff(five, n).as_secs()).collect();
    assert_eq!(schedule, vec![5, 10, 20, 30, 30, 30]);
    // The cap scales with the base, and a huge count cannot overflow.
    let one = Duration::from_secs(1);
    assert_eq!(gate_backoff(one, 0), one);
    assert_eq!(gate_backoff(one, 2), Duration::from_secs(4));
    assert_eq!(gate_backoff(one, 3), Duration::from_secs(6));
    assert_eq!(gate_backoff(one, u32::MAX), Duration::from_secs(6));
    let floor = Duration::from_millis(100);
    assert_eq!(gate_backoff(floor, 9), Duration::from_millis(600));
}
