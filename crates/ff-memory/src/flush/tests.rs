use super::*;

#[test]
fn unflushed_session_has_no_record() {
    let ledger = FlushLedger::open_in_memory().unwrap();
    assert_eq!(ledger.last_flush("s1").unwrap(), None);
}

#[test]
fn record_then_read_round_trips_and_overwrites() {
    let ledger = FlushLedger::open_in_memory().unwrap();
    ledger.record_flush("s1", 10, 1_000).unwrap();
    assert_eq!(
        ledger.last_flush("s1").unwrap(),
        Some(FlushRecord {
            message_count: 10,
            flushed_at_ms: 1_000,
        })
    );
    // A later flush overwrites the prior cycle marker.
    ledger.record_flush("s1", 25, 2_000).unwrap();
    assert_eq!(
        ledger.last_flush("s1").unwrap(),
        Some(FlushRecord {
            message_count: 25,
            flushed_at_ms: 2_000,
        })
    );
    // Sessions are independent.
    assert_eq!(ledger.last_flush("s2").unwrap(), None);
}
