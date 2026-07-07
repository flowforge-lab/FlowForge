use super::*;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn doubles_from_base() {
    let mut b = Backoff::new(ms(500), ms(30_000));
    assert_eq!(b.next_delay(), ms(500));
    assert_eq!(b.next_delay(), ms(1_000));
    assert_eq!(b.next_delay(), ms(2_000));
    assert_eq!(b.next_delay(), ms(4_000));
}

#[test]
fn clamps_at_max() {
    let mut b = Backoff::new(ms(500), ms(3_000));
    let seq: Vec<_> = (0..6).map(|_| b.next_delay()).collect();
    assert_eq!(
        seq,
        vec![
            ms(500),
            ms(1_000),
            ms(2_000),
            ms(3_000),
            ms(3_000),
            ms(3_000)
        ]
    );
}

#[test]
fn reset_returns_to_base() {
    let mut b = Backoff::new(ms(500), ms(30_000));
    b.next_delay();
    b.next_delay();
    b.reset();
    assert_eq!(b.next_delay(), ms(500));
}

#[test]
fn extreme_attempt_count_saturates_to_max() {
    let mut b = Backoff::new(ms(500), ms(30_000));
    for _ in 0..100 {
        let d = b.next_delay();
        assert!(d <= ms(30_000));
    }
}
