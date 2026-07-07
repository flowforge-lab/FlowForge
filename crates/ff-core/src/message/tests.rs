use super::*;

#[test]
fn stop_reason_wire_round_trips_every_variant() {
    // The persisted wire string and the parser must stay in lockstep for every
    // variant, so a row written by `as_wire` always reads back via `from_wire`.
    for reason in [
        StopReason::Cancelled,
        StopReason::ToolLimit,
        StopReason::Stall,
        StopReason::EmptyResponse,
        StopReason::Interrupted,
    ] {
        assert_eq!(
            StopReason::from_wire(reason.as_wire()),
            Some(reason),
            "round-trip failed for {reason:?}"
        );
    }
    // The interrupted contract, pinned explicitly.
    assert_eq!(StopReason::Interrupted.as_wire(), "interrupted");
    assert_eq!(StopReason::Interrupted.marker(), "[stopped: interrupted]");
    // Unknown wire values are a forward-compat `None`, not a panic.
    assert_eq!(StopReason::from_wire("nope"), None);
}
