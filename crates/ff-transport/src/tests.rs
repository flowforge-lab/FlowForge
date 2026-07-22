use crate::approver::MessagingApprover;
use crate::channel_map::ChannelMap;
use crate::types::ChannelId;
use ff_agent::Approver;
use ff_core::Mode;
use ff_tools::Safety;
use tempfile::TempDir;

// ── MessagingApprover ────────────────────────────────────────────────────────

#[tokio::test]
async fn act_mode_approves_write_and_sensitive() {
    let a = MessagingApprover::new(Mode::Act);
    let v = serde_json::json!({});
    assert!(a.approve("m", "c", "bash", Safety::Write, &v).await);
    assert!(a.approve("m", "c", "bash", Safety::Sensitive, &v).await);
    assert!(!a.approve("m", "c", "bash", Safety::Dangerous, &v).await);
    // #1051: a messaging-triggered agent has no interactive surface to confirm
    // a remote publish, so Publish is blocked unattended — like Dangerous.
    assert!(!a.approve("m", "c", "bash", Safety::Publish, &v).await);
}

#[tokio::test]
async fn plan_mode_denies_all() {
    let a = MessagingApprover::new(Mode::Plan);
    let v = serde_json::json!({});
    assert!(!a.approve("m", "c", "bash", Safety::Write, &v).await);
    assert!(!a.approve("m", "c", "bash", Safety::Sensitive, &v).await);
    assert!(!a.approve("m", "c", "bash", Safety::Dangerous, &v).await);
}

// ── ChannelMap ───────────────────────────────────────────────────────────────

#[test]
fn channel_map_round_trip_persistence() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("transports/channel_map.json");

    let channel = ChannelId::new("slack", "C123");
    {
        let mut map = ChannelMap::open(&path);
        assert!(map.is_empty());
        map.insert(channel.clone(), "session-abc".into());
        assert_eq!(map.get(&channel), Some("session-abc"));
    }
    // Reload from disk.
    let map = ChannelMap::open(&path);
    assert_eq!(map.get(&channel), Some("session-abc"));
    assert_eq!(map.len(), 1);
}

#[test]
fn channel_map_in_memory() {
    let mut map = ChannelMap::new();
    let ch = ChannelId::new("test", "ch1");
    map.insert(ch.clone(), "s1".into());
    assert_eq!(map.get(&ch), Some("s1"));
}
