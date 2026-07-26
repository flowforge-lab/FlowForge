use super::*;

#[test]
fn first_user_message_auto_titles_untitled_session() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    assert!(s.title.is_none());

    store.add_message(&s.id, Role::User, "fix the parser bug".into());
    let titled = store.list_sessions().into_iter().next().unwrap();
    assert_eq!(titled.title.as_deref(), Some("Fix the"));
}

#[test]
fn second_user_message_does_not_retitle() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "fix the parser bug".into());
    store.add_message(&s.id, Role::User, "now ship it".into());
    let titled = store.list_sessions().into_iter().next().unwrap();
    assert_eq!(titled.title.as_deref(), Some("Fix the"));
}

#[test]
fn assistant_first_message_does_not_title() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::Assistant, "hi there".into());
    let after = store.list_sessions().into_iter().next().unwrap();
    assert!(after.title.is_none());
}

#[test]
fn set_title_overrides_and_persists() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "fix the parser bug".into());
    store.set_title(&s.id, "Custom name".into());
    let titled = store.list_sessions().into_iter().next().unwrap();
    assert_eq!(titled.title.as_deref(), Some("Custom name"));
}

#[test]
fn set_title_unknown_session_is_noop() {
    let store = SessionStore::new();
    store.set_title("nope", "x".into());
    assert!(store.list_sessions().is_empty());
}

#[test]
fn delete_session_removes_session_and_messages() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hello".into());
    assert_eq!(store.list_sessions().len(), 1);

    assert!(store.delete_session(&s.id));
    assert!(store.list_sessions().is_empty());
    assert!(store.get_messages(&s.id).is_empty());
}

#[test]
fn delete_session_unknown_is_noop() {
    let store = SessionStore::new();
    store.create_session(None);
    assert!(!store.delete_session("nope"));
    assert_eq!(store.list_sessions().len(), 1);
}

#[test]
fn session_and_message_roundtrip() {
    let store = SessionStore::new();
    let s = store.create_session(Some("fix bug".into()));
    assert_eq!(store.list_sessions().len(), 1);

    store.add_message(&s.id, Role::User, "hello".into());
    store.add_message(&s.id, Role::Assistant, "hi".into());

    let msgs = store.get_messages(&s.id);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].role, Role::User);
    assert_eq!(msgs[1].content, "hi");
}

#[test]
fn message_attachments_round_trip() {
    use ff_core::{AttachmentKind, AttachmentSource};
    let store = SessionStore::new();
    let s = store.create_session(None);
    let attachments = vec![
        Attachment {
            kind: AttachmentKind::Image,
            media_type: "image/png".into(),
            source: AttachmentSource::Path("/tmp/shot.png".into()),
            name: Some("shot.png".into()),
            bytes: 2048,
        },
        Attachment {
            kind: AttachmentKind::Document,
            media_type: "application/pdf".into(),
            source: AttachmentSource::Inline("JVBERi0=".into()),
            name: None,
            bytes: 8,
        },
    ];
    store.add_message_with_attachments(
        &s.id,
        Role::User,
        "look at these".into(),
        attachments.clone(),
    );

    let msgs = store.get_messages(&s.id);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "look at these");
    assert_eq!(msgs[0].attachments.as_deref(), Some(attachments.as_slice()));
}

#[test]
fn plain_message_has_no_attachments() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hi".into());
    assert!(store.get_messages(&s.id)[0].attachments.is_none());
}

#[test]
fn empty_attachments_persist_as_none() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message_with_attachments(&s.id, Role::User, "hi".into(), vec![]);
    assert!(store.get_messages(&s.id)[0].attachments.is_none());
}

#[test]
fn fork_session_copies_attachments() {
    use ff_core::{AttachmentKind, AttachmentSource};
    let store = SessionStore::new();
    let s = store.create_session(None);
    let att = Attachment {
        kind: AttachmentKind::Image,
        media_type: "image/jpeg".into(),
        source: AttachmentSource::Path("/tmp/a.jpg".into()),
        name: None,
        bytes: 10,
    };
    store.add_message_with_attachments(&s.id, Role::User, "see".into(), vec![att.clone()]);

    let forked = store.fork_session(&s.id).unwrap();
    let msgs = store.get_messages(&forked.id);
    assert_eq!(msgs[0].attachments.as_deref(), Some([att].as_slice()));
}

#[test]
fn reconcile_relabels_orphaned_empty_assistant_row() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hi".into());
    // The row the agent loop reserves and never finalizes.
    store.add_message(&s.id, Role::Assistant, String::new());

    let changed = store.reconcile_orphaned_assistant_rows(&s.id, "[stopped: interrupted]");
    assert_eq!(changed, 1);
    let msgs = store.get_messages(&s.id);
    assert_eq!(msgs[1].content, "[stopped: interrupted]");
    // The sweep stamps the structured reason inline in the same UPDATE, so the
    // reconciled row classifies on the FE without the legacy string match.
    assert_eq!(msgs[1].stop_reason, Some(StopReason::Interrupted));
}

#[test]
fn reconcile_leaves_non_orphan_rows_untouched() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    // A normal answer.
    store.add_message(&s.id, Role::Assistant, "real answer".into());
    // A tool-call-only row (empty content but legitimately carries tool calls).
    let tc = store.add_message(&s.id, Role::Assistant, String::new());
    store.attach_tool_calls(
        &tc.id,
        &s.id,
        vec![ToolCall {
            id: "call_1".into(),
            name: "view".into(),
            arguments: "{}".into(),
        }],
    );
    // A reasoning-only reserved row (mid-stream: CoT arrived before content).
    let r = store.add_message(&s.id, Role::Assistant, String::new());
    store.set_message_reasoning(&r.id, &s.id, "thinking");
    // An empty user row must never be relabeled.
    store.add_message(&s.id, Role::User, String::new());

    let changed = store.reconcile_orphaned_assistant_rows(&s.id, "[stopped: interrupted]");
    assert_eq!(changed, 0, "no genuine orphan present");
    let msgs = store.get_messages(&s.id);
    assert_eq!(msgs[0].content, "real answer");
    assert_eq!(msgs[1].content, "");
    assert!(msgs[1].tool_calls.is_some());
    assert_eq!(msgs[2].content, "");
    assert_eq!(msgs[2].reasoning.as_deref(), Some("thinking"));
    assert_eq!(msgs[3].content, "");
}

#[test]
fn draft_session_defers_persistence_until_first_message() {
    let store = SessionStore::new();
    let draft = store.create_draft_session();
    // Not on disk yet: absent from the list, but still resolvable as a draft.
    assert!(store.list_sessions().is_empty());
    assert_eq!(
        store.get_session(&draft.id).map(|s| s.id),
        Some(draft.id.clone()),
    );

    // The first user message flushes the draft: it now persists, lists, and
    // carries the auto-title heuristic (#671 item 2a).
    store.add_message(&draft.id, Role::User, "summarize the parser".into());
    let listed = store.list_sessions();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, draft.id);
    assert!(
        listed[0].title.is_some(),
        "first message seeds the auto-title"
    );
}

#[test]
fn untouched_draft_leaves_no_row() {
    let store = SessionStore::new();
    let draft = store.create_draft_session();
    // Never messaged or configured: nothing is persisted (a fresh store stands
    // in for a fresh app start — the in-memory draft is simply gone).
    assert!(store.list_sessions().is_empty());
    // Deleting the abandoned draft succeeds and clears the pending entry.
    assert!(store.delete_session(&draft.id));
    assert!(store.get_session(&draft.id).is_none());
}

#[test]
fn config_write_flushes_a_draft() {
    let store = SessionStore::new();
    let draft = store.create_draft_session();
    // Picking a model before the first message must not be lost: the write
    // flushes the draft first, then lands the override on the real row.
    let sel = model_sel("openai-main", "gpt-4o");
    store.set_session_model(&draft.id, Some(sel.clone()));
    assert_eq!(store.list_sessions().len(), 1);
    assert_eq!(store.session_model(&draft.id), Some(sel));
}

#[test]
fn create_session_still_persists_immediately() {
    // The direct store API (CLI, background flows) is unchanged: create_session
    // writes a row right away — only the desktop `＋` uses the deferred draft.
    let store = SessionStore::new();
    let s = store.create_session(None);
    assert_eq!(store.list_sessions().len(), 1);
    assert_eq!(store.list_sessions()[0].id, s.id);
}

#[test]
fn set_message_reasoning_round_trips() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    let m = store.add_message(&s.id, Role::Assistant, String::new());
    store.set_message_reasoning(&m.id, &s.id, "step 1: multiply 17 by 23");

    let msgs = store.get_messages(&s.id);
    assert_eq!(
        msgs[0].reasoning.as_deref(),
        Some("step 1: multiply 17 by 23")
    );
}

#[test]
fn set_message_reasoning_is_returned_by_set_message_content() {
    // run_turn persists reasoning, then finalizes content; the finalized
    // Message must carry both (#375 PR-1).
    let store = SessionStore::new();
    let s = store.create_session(None);
    let m = store.add_message(&s.id, Role::Assistant, String::new());
    store.set_message_reasoning(&m.id, &s.id, "thinking...");
    let finalized = store.set_message_content(&m.id, &s.id, "answer".into());
    assert_eq!(finalized.content, "answer");
    assert_eq!(finalized.reasoning.as_deref(), Some("thinking..."));
}

#[test]
fn plain_message_has_no_reasoning() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::Assistant, "hi".into());
    assert!(store.get_messages(&s.id)[0].reasoning.is_none());
}

#[test]
fn set_message_reasoning_unknown_message_is_noop() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.set_message_reasoning("nope", &s.id, "orphan");
    assert!(store.get_messages(&s.id).is_empty());
}

#[test]
fn reasoning_round_trips_through_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let sid;
    let mid;
    {
        let store = SessionStore::open(&path).unwrap();
        let s = store.create_session(None);
        sid = s.id.clone();
        let m = store.add_message(&s.id, Role::Assistant, "answer".into());
        mid = m.id.clone();
        store.set_message_reasoning(&mid, &sid, "chain of thought");
    }
    let store = SessionStore::open(&path).unwrap();
    let msgs = store.get_messages(&sid);
    assert_eq!(msgs[0].id, mid);
    assert_eq!(msgs[0].reasoning.as_deref(), Some("chain of thought"));
}

#[test]
fn fork_session_copies_reasoning() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    let m = store.add_message(&s.id, Role::Assistant, "a".into());
    store.set_message_reasoning(&m.id, &s.id, "because");

    let forked = store.fork_session(&s.id).unwrap();
    let msgs = store.get_messages(&forked.id);
    assert_eq!(msgs[0].reasoning.as_deref(), Some("because"));
}

#[test]
fn migration_v3_to_v4_preserves_messages_and_adds_reasoning() {
    // A v3 database (pre-reasoning) must gain the column on open without
    // losing existing rows, and old rows read back with reasoning = None.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let sid = "sess-1";
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id TEXT PRIMARY KEY, goal TEXT, title TEXT, summary TEXT,
                 status TEXT NOT NULL, created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL, phenotype TEXT, mode TEXT, workspace TEXT
             );
             CREATE TABLE messages (
                 id TEXT PRIMARY KEY, session_id TEXT NOT NULL, seq INTEGER NOT NULL,
                 role TEXT NOT NULL, content TEXT NOT NULL, tool_calls TEXT,
                 tool_call_id TEXT, attachments TEXT, created_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, status, created_at, updated_at)
             VALUES (?1, 'active', 0, 0)",
            params![sid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (id, session_id, seq, role, content, created_at)
             VALUES ('m1', ?1, 0, 'assistant', 'legacy', 0)",
            params![sid],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();
    }

    let store = SessionStore::open(&path).unwrap();
    let msgs = store.get_messages(sid);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "legacy");
    assert!(msgs[0].reasoning.is_none());

    let version: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 11);
}

#[test]
fn put_and_get_compaction_original_round_trip() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    let m = store.add_tool_result_message(&s.id, "call-1".into(), "compressed".into());
    store.put_compaction_original(&s.id, &m.id, "abc123", "the full original blob");
    assert_eq!(
        store.compaction_original("abc123").as_deref(),
        Some("the full original blob")
    );
    assert!(store.compaction_original("missing").is_none());
}

#[test]
fn put_compaction_original_is_idempotent_on_key() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.put_compaction_original(&s.id, "m1", "k", "first");
    store.put_compaction_original(&s.id, "m2", "k", "second");
    assert_eq!(store.compaction_original("k").as_deref(), Some("first"));
}

#[test]
fn compaction_originals_cascade_on_session_delete() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.put_compaction_original(&s.id, "m1", "k", "blob");
    assert!(store.compaction_original("k").is_some());
    assert!(store.delete_session(&s.id));
    assert!(
        store.compaction_original("k").is_none(),
        "deleting a session must cascade-drop its compaction originals"
    );
}

#[test]
fn rehome_compaction_original_survives_child_session_delete() {
    // #469: a sub-agent stashes originals under its ephemeral child session.
    // Re-homing a marker's original to the parent before the child is deleted
    // must keep it retrievable; a sibling original left behind must still cascade.
    let store = SessionStore::new();
    let parent = store.create_session(None);
    let child = store.create_session(None);
    store.put_compaction_original(&child.id, "m1", "kept", "the kept original");
    store.put_compaction_original(&child.id, "m2", "dropped", "the dropped original");

    store.rehome_compaction_original("kept", &parent.id);
    assert!(store.delete_session(&child.id));

    assert_eq!(
        store.compaction_original("kept").as_deref(),
        Some("the kept original"),
        "a re-homed original must survive the child session teardown"
    );
    assert!(
        store.compaction_original("dropped").is_none(),
        "an original left on the child session must still cascade away"
    );
}

#[test]
fn rehome_compaction_original_unknown_key_is_noop() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.rehome_compaction_original("nope", &s.id);
    assert!(store.compaction_original("nope").is_none());
}

#[test]
fn migration_v4_to_v5_preserves_messages_and_creates_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let sid = "sess-1";
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id TEXT PRIMARY KEY, goal TEXT, title TEXT, summary TEXT,
                 status TEXT NOT NULL, created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL, phenotype TEXT, mode TEXT, workspace TEXT
             );
             CREATE TABLE messages (
                 id TEXT PRIMARY KEY, session_id TEXT NOT NULL, seq INTEGER NOT NULL,
                 role TEXT NOT NULL, content TEXT NOT NULL, tool_calls TEXT,
                 tool_call_id TEXT, attachments TEXT, reasoning TEXT,
                 created_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, status, created_at, updated_at)
             VALUES (?1, 'active', 0, 0)",
            params![sid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (id, session_id, seq, role, content, created_at)
             VALUES ('m1', ?1, 0, 'assistant', 'legacy', 0)",
            params![sid],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 4).unwrap();
    }

    let store = SessionStore::open(&path).unwrap();
    let msgs = store.get_messages(sid);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "legacy");

    store.put_compaction_original(sid, "m1", "k", "blob");
    assert_eq!(store.compaction_original("k").as_deref(), Some("blob"));

    let version: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 11);
}

#[test]
fn migration_v5_to_v6_preserves_session_and_adds_model() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let sid = "sess-1";
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id TEXT PRIMARY KEY, goal TEXT, title TEXT, summary TEXT,
                 status TEXT NOT NULL, created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL, phenotype TEXT, mode TEXT, workspace TEXT
             );
             CREATE TABLE messages (
                 id TEXT PRIMARY KEY, session_id TEXT NOT NULL, seq INTEGER NOT NULL,
                 role TEXT NOT NULL, content TEXT NOT NULL, tool_calls TEXT,
                 tool_call_id TEXT, attachments TEXT, reasoning TEXT,
                 created_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, goal, status, created_at, updated_at)
             VALUES (?1, 'legacy goal', 'active', 0, 0)",
            params![sid],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 5).unwrap();
    }

    let store = SessionStore::open(&path).unwrap();
    // The pre-existing row survives the ALTER with a NULL (inherited) model.
    let s = store.get_session(sid).unwrap();
    assert_eq!(s.goal.as_deref(), Some("legacy goal"));
    assert!(s.model.is_none());
    // ...and the new column is writable post-upgrade.
    store.set_session_model(sid, Some(model_sel("openai-main", "gpt-4o")));
    assert_eq!(
        store.session_model(sid),
        Some(model_sel("openai-main", "gpt-4o"))
    );

    let version: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 11);
}

#[test]
fn fork_session_clones_messages_with_fresh_ids() {
    let store = SessionStore::new();
    let s = store.create_session(Some("fix bug".into()));
    store.add_message(&s.id, Role::User, "hello".into());
    store.add_message(&s.id, Role::Assistant, "hi".into());

    let forked = store.fork_session(&s.id).unwrap();
    assert_ne!(forked.id, s.id);

    let src_msgs = store.get_messages(&s.id);
    let fork_msgs = store.get_messages(&forked.id);
    assert_eq!(fork_msgs.len(), src_msgs.len());
    for (orig, copy) in src_msgs.iter().zip(&fork_msgs) {
        assert_ne!(copy.id, orig.id);
        assert_eq!(copy.session_id, forked.id);
        assert_eq!(copy.role, orig.role);
        assert_eq!(copy.content, orig.content);
    }
}

#[test]
fn fork_session_titled_gets_copy_suffix() {
    let store = SessionStore::new();
    let titled = store.create_session(None);
    store.set_title(&titled.id, "Fix bug".into());
    let forked = store.fork_session(&titled.id).unwrap();
    assert_eq!(forked.title.as_deref(), Some("Fix bug (copy)"));

    let untitled = store.create_session(None);
    let forked_untitled = store.fork_session(&untitled.id).unwrap();
    assert!(forked_untitled.title.is_none());
}

#[test]
fn fork_session_unknown_returns_none() {
    let store = SessionStore::new();
    assert!(store.fork_session("nope").is_none());
}

#[test]
fn fork_session_leaves_source_untouched() {
    let store = SessionStore::new();
    let s = store.create_session(Some("keep me".into()));
    store.add_message(&s.id, Role::User, "original".into());
    let before = store.get_messages(&s.id);

    store.fork_session(&s.id).unwrap();

    let after = store.get_messages(&s.id);
    assert_eq!(after.len(), before.len());
    assert_eq!(after[0].id, before[0].id);
    assert_eq!(after[0].session_id, s.id);
    assert_eq!(store.list_sessions().len(), 2);
}

#[test]
fn assistant_message_is_stamped_with_the_session_phenotype() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.set_session_phenotype(&s.id, Some("codon".into()));

    store.add_message(&s.id, Role::User, "hi".into());
    let reserved = store.add_message(&s.id, Role::Assistant, String::new());
    // The returned reserved row already carries the stamped author, so events
    // emitted from it surface the true author too.
    assert_eq!(reserved.author_name.as_deref(), Some("codon"));

    let msgs = store.get_messages(&s.id);
    let user = msgs.iter().find(|m| m.role == Role::User).unwrap();
    let assistant = msgs.iter().find(|m| m.role == Role::Assistant).unwrap();
    // Only the assistant row is stamped; the user row is left to live resolution.
    assert_eq!(user.author_name, None);
    assert_eq!(assistant.author_name.as_deref(), Some("codon"));
}

#[test]
fn assistant_message_in_an_unbound_session_has_no_author() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    let m = store.add_message(&s.id, Role::Assistant, "ok".into());
    assert_eq!(m.author_name, None);
    assert_eq!(store.get_messages(&s.id)[0].author_name, None);
}

#[test]
fn fork_preserves_the_persisted_author() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.set_session_phenotype(&s.id, Some("codon".into()));
    store.add_message(&s.id, Role::Assistant, "authored by codon".into());

    let forked = store.fork_session(&s.id).unwrap();
    assert_eq!(
        store.get_messages(&forked.id)[0].author_name.as_deref(),
        Some("codon")
    );
}

#[test]
fn new_session_has_no_phenotype_binding() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    assert!(s.phenotype.is_none());
    assert!(store.session_phenotype(&s.id).is_none());
}

#[test]
fn set_and_clear_session_phenotype() {
    let store = SessionStore::new();
    let s = store.create_session(None);

    store.set_session_phenotype(&s.id, Some("codon".into()));
    assert_eq!(store.session_phenotype(&s.id).as_deref(), Some("codon"));

    store.set_session_phenotype(&s.id, None);
    assert!(store.session_phenotype(&s.id).is_none());
}

#[test]
fn set_session_phenotype_unknown_session_is_noop() {
    let store = SessionStore::new();
    store.set_session_phenotype("nope", Some("codon".into()));
    assert!(store.session_phenotype("nope").is_none());
}

#[test]
fn fork_session_copies_phenotype_binding() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.set_session_phenotype(&s.id, Some("codon".into()));

    let forked = store.fork_session(&s.id).unwrap();
    assert_eq!(forked.phenotype.as_deref(), Some("codon"));
    // Changing the fork's binding does not touch the source.
    store.set_session_phenotype(&forked.id, Some("default".into()));
    assert_eq!(store.session_phenotype(&s.id).as_deref(), Some("codon"));
}

#[test]
fn new_session_has_no_workspace() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    assert!(s.workspace.is_none());
    assert!(store.session_workspace(&s.id).is_none());
}

#[test]
fn set_and_clear_session_workspace() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.set_session_workspace(&s.id, Some("/work/proj".into()));
    assert_eq!(
        store.session_workspace(&s.id).as_deref(),
        Some("/work/proj")
    );
    store.set_session_workspace(&s.id, None);
    assert!(store.session_workspace(&s.id).is_none());
}

#[test]
fn set_session_workspace_unknown_session_is_noop() {
    let store = SessionStore::new();
    store.set_session_workspace("nope", Some("/work/proj".into()));
    assert!(store.session_workspace("nope").is_none());
}

#[test]
fn fork_session_copies_workspace() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.set_session_workspace(&s.id, Some("/work/proj".into()));

    let forked = store.fork_session(&s.id).unwrap();
    assert_eq!(forked.workspace.as_deref(), Some("/work/proj"));
    // Changing the fork's cwd does not touch the source.
    store.set_session_workspace(&forked.id, Some("/work/other".into()));
    assert_eq!(
        store.session_workspace(&s.id).as_deref(),
        Some("/work/proj")
    );
}

#[test]
fn new_session_has_no_mode_binding() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    assert!(s.mode.is_none());
    assert!(store.session_mode(&s.id).is_none());
}

#[test]
fn set_and_clear_session_mode() {
    let store = SessionStore::new();
    let s = store.create_session(None);

    store.set_session_mode(&s.id, Some(Mode::Plan));
    assert_eq!(store.session_mode(&s.id), Some(Mode::Plan));

    store.set_session_mode(&s.id, None);
    assert!(store.session_mode(&s.id).is_none());
}

#[test]
fn set_session_mode_unknown_session_is_noop() {
    let store = SessionStore::new();
    store.set_session_mode("nope", Some(Mode::Act));
    assert!(store.session_mode("nope").is_none());
}

#[test]
fn fork_session_copies_mode_binding() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.set_session_mode(&s.id, Some(Mode::Act));

    let forked = store.fork_session(&s.id).unwrap();
    assert_eq!(forked.mode, Some(Mode::Act));
    // Changing the fork's binding does not touch the source.
    store.set_session_mode(&forked.id, Some(Mode::Auto));
    assert_eq!(store.session_mode(&s.id), Some(Mode::Act));
}

fn model_sel(connection: &str, model: &str) -> ModelSelection {
    ModelSelection {
        connection: connection.into(),
        model: model.into(),
    }
}

#[test]
fn new_session_has_no_model_binding() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    assert!(s.model.is_none());
    assert!(store.session_model(&s.id).is_none());
}

#[test]
fn set_and_clear_session_model() {
    let store = SessionStore::new();
    let s = store.create_session(None);

    let sel = model_sel("openai-main", "gpt-4o");
    store.set_session_model(&s.id, Some(sel.clone()));
    assert_eq!(store.session_model(&s.id), Some(sel));

    store.set_session_model(&s.id, None);
    assert!(store.session_model(&s.id).is_none());
}

#[test]
fn set_session_model_unknown_session_is_noop() {
    let store = SessionStore::new();
    store.set_session_model("nope", Some(model_sel("openai-main", "gpt-4o")));
    assert!(store.session_model("nope").is_none());
}

#[test]
fn fork_session_copies_model_binding() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    let sel = model_sel("openai-main", "gpt-4o");
    store.set_session_model(&s.id, Some(sel.clone()));

    let forked = store.fork_session(&s.id).unwrap();
    assert_eq!(forked.model, Some(sel.clone()));
    // Changing the fork's binding does not touch the source.
    store.set_session_model(&forked.id, Some(model_sel("anthropic", "claude")));
    assert_eq!(store.session_model(&s.id), Some(sel));
}

// --- Session-tier MCP overrides (RFC 0018 C3, #590) ---

fn ws_server(id: &str) -> McpServerConfig {
    McpServerConfig {
        id: id.into(),
        command: "codegraph".into(),
        args: vec!["serve".into(), "--mcp".into()],
        env: Default::default(),
        disabled: false,
        scope: ff_core::McpScope::Workspace,
        reaches_network: None,
    }
}

#[test]
fn new_session_has_no_mcp_servers_binding() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    assert!(s.mcp_servers.is_none());
    assert!(store.session_mcp_servers(&s.id).is_none());
}

#[test]
fn set_and_clear_session_mcp_servers() {
    let store = SessionStore::new();
    let s = store.create_session(None);

    let servers = vec![ws_server("codegraph")];
    store.set_session_mcp_servers(&s.id, Some(servers.clone()));
    assert_eq!(store.session_mcp_servers(&s.id), Some(servers));

    store.set_session_mcp_servers(&s.id, None);
    assert!(store.session_mcp_servers(&s.id).is_none());
}

#[test]
fn set_session_mcp_servers_unknown_session_is_noop() {
    let store = SessionStore::new();
    store.set_session_mcp_servers("nope", Some(vec![ws_server("codegraph")]));
    assert!(store.session_mcp_servers("nope").is_none());
}

#[test]
fn fork_session_copies_mcp_servers_binding() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    let servers = vec![ws_server("codegraph")];
    store.set_session_mcp_servers(&s.id, Some(servers.clone()));

    let forked = store.fork_session(&s.id).unwrap();
    assert_eq!(forked.mcp_servers, Some(servers.clone()));
    // Changing the fork's binding does not touch the source.
    store.set_session_mcp_servers(&forked.id, Some(vec![ws_server("other")]));
    assert_eq!(store.session_mcp_servers(&s.id), Some(servers));
}

#[test]
fn migration_v6_to_v7_preserves_session_and_adds_mcp_servers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let sid = "sess-1";
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id TEXT PRIMARY KEY, goal TEXT, title TEXT, summary TEXT,
                 status TEXT NOT NULL, created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL, phenotype TEXT, mode TEXT,
                 workspace TEXT, model TEXT
             );
             CREATE TABLE messages (
                 id TEXT PRIMARY KEY, session_id TEXT NOT NULL, seq INTEGER NOT NULL,
                 role TEXT NOT NULL, content TEXT NOT NULL, tool_calls TEXT,
                 tool_call_id TEXT, attachments TEXT, reasoning TEXT,
                 created_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, goal, status, created_at, updated_at)
             VALUES (?1, 'legacy goal', 'active', 0, 0)",
            params![sid],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 6).unwrap();
    }

    let store = SessionStore::open(&path).unwrap();
    // The pre-existing v6 row survives the ALTER with a NULL (inherited) set.
    let s = store.get_session(sid).unwrap();
    assert_eq!(s.goal.as_deref(), Some("legacy goal"));
    assert!(s.mcp_servers.is_none());
    // ...and the new column is writable post-upgrade.
    let servers = vec![ws_server("codegraph")];
    store.set_session_mcp_servers(sid, Some(servers.clone()));
    assert_eq!(store.session_mcp_servers(sid), Some(servers));

    let version: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 11);
}

#[test]
fn migration_v7_to_v9_preserves_messages_and_adds_stop_reason() {
    // A v7 database (pre-#657/#658) must gain both the author_name (v8) and
    // stop_reason (v9) columns on open
    // without losing existing rows, and old rows read back with stop_reason =
    // None -- the read-time classifier is their only fallback.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let sid = "sess-1";
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 id TEXT PRIMARY KEY, goal TEXT, title TEXT, summary TEXT,
                 status TEXT NOT NULL, created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL, phenotype TEXT, mode TEXT,
                 workspace TEXT, model TEXT, mcp_servers TEXT
             );
             CREATE TABLE messages (
                 id TEXT PRIMARY KEY, session_id TEXT NOT NULL, seq INTEGER NOT NULL,
                 role TEXT NOT NULL, content TEXT NOT NULL, tool_calls TEXT,
                 tool_call_id TEXT, attachments TEXT, reasoning TEXT,
                 created_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, status, created_at, updated_at)
             VALUES (?1, 'active', 0, 0)",
            params![sid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (id, session_id, seq, role, content, created_at)
             VALUES ('m1', ?1, 0, 'assistant', '[stopped]', 0)",
            params![sid],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 7).unwrap();
    }

    let store = SessionStore::open(&path).unwrap();
    // The pre-existing v7 row survives the ALTER with a NULL stop_reason.
    let msgs = store.get_messages(sid);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "[stopped]");
    assert!(msgs[0].stop_reason.is_none());

    // ...and the new column is writable post-upgrade, round-tripping the variant.
    store.set_message_stop_reason(&msgs[0].id, sid, StopReason::ToolLimit);
    let reloaded = store.get_messages(sid);
    assert_eq!(reloaded[0].stop_reason, Some(StopReason::ToolLimit));

    let version: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 11);
}

#[test]
fn fork_session_copies_stop_reason() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    let m = store.add_message(&s.id, Role::Assistant, "[stopped]".into());
    store.set_message_stop_reason(&m.id, &s.id, StopReason::Cancelled);

    let forked = store.fork_session(&s.id).unwrap();
    let msgs = store.get_messages(&forked.id);
    assert_eq!(msgs[0].stop_reason, Some(StopReason::Cancelled));
}

// --- SQLite-backed persistence (RFC 0012 / #276) ---

#[test]
fn disk_store_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");

    let sid;
    let mid;
    {
        let store = SessionStore::open(&path).unwrap();
        let s = store.create_session(Some("durable goal".into()));
        sid = s.id.clone();
        store.set_title(&s.id, "Durable".into());
        store.set_session_phenotype(&s.id, Some("codon".into()));
        store.set_session_mode(&s.id, Some(Mode::Act));
        store.set_session_workspace(&s.id, Some("/work/proj".into()));
        store.set_session_model(&s.id, Some(model_sel("openai-main", "gpt-4o")));
        let m = store.add_message(&s.id, Role::User, "remember me".into());
        mid = m.id.clone();
        store.add_message(&s.id, Role::Assistant, "noted".into());
    }

    // Reopen over the same path: state is still there.
    let store = SessionStore::open(&path).unwrap();
    let sessions = store.list_sessions();
    assert_eq!(sessions.len(), 1);
    let reopened = &sessions[0];
    assert_eq!(reopened.id, sid);
    assert_eq!(reopened.title.as_deref(), Some("Durable"));
    assert_eq!(reopened.goal.as_deref(), Some("durable goal"));
    assert_eq!(store.session_phenotype(&sid).as_deref(), Some("codon"));
    assert_eq!(store.session_mode(&sid), Some(Mode::Act));
    assert_eq!(store.session_workspace(&sid).as_deref(), Some("/work/proj"));
    assert_eq!(
        store.session_model(&sid),
        Some(model_sel("openai-main", "gpt-4o"))
    );

    let msgs = store.get_messages(&sid);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].id, mid);
    assert_eq!(msgs[0].content, "remember me");
    assert_eq!(msgs[1].role, Role::Assistant);
}

#[test]
fn fork_session_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");

    let source_id;
    let forked_id;
    {
        let store = SessionStore::open(&path).unwrap();
        let s = store.create_session(Some("durable fork".into()));
        source_id = s.id.clone();
        store.set_title(&s.id, "Durable fork".into());
        store.add_message(&s.id, Role::User, "copy me".into());
        store.add_message(&s.id, Role::Assistant, "copied".into());

        let forked = store.fork_session(&s.id).unwrap();
        forked_id = forked.id;
    }

    let store = SessionStore::open(&path).unwrap();
    let source_msgs = store.get_messages(&source_id);
    let forked_msgs = store.get_messages(&forked_id);
    assert_eq!(forked_msgs.len(), source_msgs.len());
    assert_eq!(forked_msgs[0].content, "copy me");
    assert_eq!(forked_msgs[1].content, "copied");
    assert_eq!(forked_msgs[0].session_id, forked_id);
    assert_ne!(forked_msgs[0].id, source_msgs[0].id);

    let forked = store.get_session(&forked_id).unwrap();
    assert_eq!(forked.title.as_deref(), Some("Durable fork (copy)"));
}

#[test]
fn tool_calls_round_trip_through_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let calls = vec![ToolCall {
        id: "call_1".into(),
        name: "bash".into(),
        arguments: r#"{"command":"ls"}"#.into(),
    }];
    let sid;
    let mid;
    {
        let store = SessionStore::open(&path).unwrap();
        let s = store.create_session(None);
        sid = s.id.clone();
        let m = store.add_message(&s.id, Role::Assistant, String::new());
        mid = m.id.clone();
        store.attach_tool_calls(&mid, &sid, calls.clone());
    }
    let store = SessionStore::open(&path).unwrap();
    let msgs = store.get_messages(&sid);
    assert_eq!(msgs[0].tool_calls.as_deref(), Some(calls.as_slice()));
}

#[test]
fn cascade_delete_removes_messages_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open(&path).unwrap();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "a".into());
    store.add_message(&s.id, Role::Assistant, "b".into());

    assert!(store.delete_session(&s.id));

    // The FK cascade must have removed the orphaned messages too.
    let count: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn messages_keep_insertion_order_via_seq() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    for i in 0..5 {
        store.add_message(&s.id, Role::User, format!("msg {i}"));
    }
    let msgs = store.get_messages(&s.id);
    let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, ["msg 0", "msg 1", "msg 2", "msg 3", "msg 4"]);
}

#[test]
fn migration_is_idempotent_across_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    SessionStore::open(&path).unwrap();
    let store = SessionStore::open(&path).unwrap();
    let version: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 11);
}

#[test]
fn export_json_round_trips() {
    let store = SessionStore::new();
    let s = store.create_session(Some("ship it".into()));
    store.add_message(&s.id, Role::User, "hello".into());
    store.add_message(&s.id, Role::Assistant, "hi there".into());

    let json = store.export_session(&s.id, Format::Json).unwrap();
    let env: ExportEnvelope = serde_json::from_str(&json).unwrap();

    assert_eq!(env.session, store.get_session(&s.id).unwrap());
    assert_eq!(env.messages, store.get_messages(&s.id));
}

#[test]
fn export_markdown_has_headings_and_folds_large_tool_output() {
    let store = SessionStore::new();
    let s = store.create_session(Some("debug".into()));
    store.add_message(&s.id, Role::User, "whats wrong".into());
    let assistant = store.add_message(&s.id, Role::Assistant, "let me check".into());
    store.attach_tool_calls(
        &assistant.id,
        &s.id,
        vec![ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: "{\"path\":\"x\"}".into(),
        }],
    );
    let big = "x".repeat(TOOL_OUTPUT_FOLD_THRESHOLD + 100);
    store.add_tool_result_message(&s.id, "call_1".into(), big);

    let md = store.export_session(&s.id, Format::Markdown).unwrap();

    assert!(md.contains("- **Goal:** debug"));
    assert!(md.contains("## You"));
    assert!(md.contains("## Assistant"));
    assert!(md.contains("## Tool"));
    assert!(md.contains("**Tool call:** read_file("));
    assert!(md.contains("<details>"));
}

#[test]
fn export_markdown_skips_system_messages() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::System, "you are a helpful agent".into());
    store.add_message(&s.id, Role::User, "hi".into());

    let md = store.export_session(&s.id, Format::Markdown).unwrap();

    assert!(!md.contains("you are a helpful agent"));
    assert!(md.contains("## You"));
}

#[test]
fn export_markdown_includes_reasoning() {
    // #549: persisted chain-of-thought is folded into the Markdown export.
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "what is 17*23?".into());
    let a = store.add_message(&s.id, Role::Assistant, "391".into());
    store.set_message_reasoning(&a.id, &s.id, "17 * 23 = 391");

    let md = store.export_session(&s.id, Format::Markdown).unwrap();

    assert!(md.contains("<details><summary>Thought</summary>"));
    assert!(md.contains("17 * 23 = 391"));
    // The answer is still present after the fold.
    assert!(md.contains("391"));
}

#[test]
fn export_unknown_session_is_none() {
    let store = SessionStore::new();
    assert!(store.export_session("nope", Format::Json).is_none());
    assert!(store.export_session("nope", Format::Markdown).is_none());
}

// --- edit_user_message (#464) ---

#[test]
fn edit_user_message_replaces_content_and_truncates_after() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    let u1 = store.add_message(&s.id, Role::User, "first question".into());
    store.add_message(&s.id, Role::Assistant, "first answer".into());
    store.add_message(&s.id, Role::User, "second question".into());
    store.add_message(&s.id, Role::Assistant, "second answer".into());

    let edited = store
        .edit_user_message(&s.id, &u1.id, "first question (edited)".into(), None)
        .unwrap();
    assert_eq!(edited, u1.id, "returns the edited message's id");

    let msgs = store.get_messages(&s.id);
    assert_eq!(
        msgs.len(),
        1,
        "old response and everything after is dropped"
    );
    assert_eq!(msgs[0].id, u1.id);
    assert_eq!(msgs[0].content, "first question (edited)");
}

#[test]
fn edit_user_message_truncation_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let session_id;
    let edited_id;
    {
        let store = SessionStore::open(&path).unwrap();
        let s = store.create_session(None);
        session_id = s.id.clone();
        let u1 = store.add_message(&s.id, Role::User, "q1".into());
        edited_id = u1.id.clone();
        store.add_message(&s.id, Role::Assistant, "a1".into());
        store.add_message(&s.id, Role::User, "q2".into());
        store
            .edit_user_message(&s.id, &u1.id, "q1 edited".into(), None)
            .unwrap();
    }
    let store = SessionStore::open(&path).unwrap();
    let msgs = store.get_messages(&session_id);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].id, edited_id);
    assert_eq!(msgs[0].content, "q1 edited");
}

#[test]
fn edit_user_message_updates_and_clears_attachments() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    let u = store.add_message(&s.id, Role::User, "look".into());

    let att = vec![Attachment {
        kind: ff_core::AttachmentKind::Image,
        media_type: "image/png".into(),
        source: ff_core::AttachmentSource::Inline("AAAA".into()),
        name: Some("shot.png".into()),
        bytes: 3,
    }];
    store
        .edit_user_message(&s.id, &u.id, "look (with image)".into(), Some(att))
        .unwrap();
    let msgs = store.get_messages(&s.id);
    assert_eq!(msgs[0].attachments.as_ref().map(Vec::len), Some(1));

    // Editing again with no attachments clears them back to NULL.
    store
        .edit_user_message(&s.id, &u.id, "look (no image)".into(), None)
        .unwrap();
    let msgs = store.get_messages(&s.id);
    assert_eq!(msgs[0].attachments, None);
}

#[test]
fn edit_user_message_rejects_non_user_message() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "q".into());
    let a = store.add_message(&s.id, Role::Assistant, "a".into());

    let err = store
        .edit_user_message(&s.id, &a.id, "tampered".into(), None)
        .unwrap_err();
    assert_eq!(err, EditMessageError::NotUserMessage);
    // Transcript untouched.
    assert_eq!(store.get_messages(&s.id).len(), 2);
    assert_eq!(store.get_messages(&s.id)[1].content, "a");
}

#[test]
fn edit_user_message_rejects_unknown_id() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "q".into());

    let err = store
        .edit_user_message(&s.id, "does-not-exist", "x".into(), None)
        .unwrap_err();
    assert_eq!(err, EditMessageError::UnknownMessage);
}

#[test]
fn edit_user_message_rejects_id_from_another_session() {
    let store = SessionStore::new();
    let a = store.create_session(None);
    let b = store.create_session(None);
    let u = store.add_message(&a.id, Role::User, "in a".into());

    // The id exists, but not in session b.
    let err = store
        .edit_user_message(&b.id, &u.id, "x".into(), None)
        .unwrap_err();
    assert_eq!(err, EditMessageError::UnknownMessage);
    // Session a is untouched.
    assert_eq!(store.get_messages(&a.id)[0].content, "in a");
}

#[test]
fn edit_user_message_drops_orphaned_compaction_originals() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    let u = store.add_message(&s.id, Role::User, "q".into());
    let a = store.add_message(&s.id, Role::Assistant, "a".into());
    store.put_compaction_original(&s.id, &a.id, "hash-key-1", "verbatim original");
    assert_eq!(
        store.compaction_original("hash-key-1").as_deref(),
        Some("verbatim original")
    );

    store
        .edit_user_message(&s.id, &u.id, "q edited".into(), None)
        .unwrap();

    // The assistant message that backed the original was truncated, so its
    // original must be gone too (no orphan).
    assert_eq!(store.compaction_original("hash-key-1"), None);
}

#[test]
fn search_messages_finds_across_sessions() {
    let store = SessionStore::new();
    let s1 = store.create_session(Some("first".into()));
    let s2 = store.create_session(Some("second".into()));
    store.set_title(&s1.id, "Rust session".into());
    store.set_title(&s2.id, "Python session".into());
    store.add_message(
        &s1.id,
        Role::User,
        "I want to learn Rust programming".into(),
    );
    store.add_message(&s1.id, Role::Assistant, "Rust is a systems language".into());
    store.add_message(&s2.id, Role::User, "Tell me about Python".into());
    store.add_message(
        &s2.id,
        Role::Assistant,
        "Python is great for scripting".into(),
    );

    let hits = store.search_messages("Rust", 10);
    assert_eq!(hits.len(), 2, "should find 2 messages mentioning Rust");
    assert!(hits.iter().all(|h| h.session_id == s1.id));

    let hits = store.search_messages("programming", 10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_title.as_deref(), Some("Rust session"));

    // Empty query returns nothing.
    assert!(store.search_messages("", 10).is_empty());
    assert!(store.search_messages("   ", 10).is_empty());
}

#[test]
fn search_in_session_scopes_to_one_session() {
    let store = SessionStore::new();
    let s1 = store.create_session(None);
    let s2 = store.create_session(None);
    store.add_message(&s1.id, Role::User, "hello world".into());
    store.add_message(&s2.id, Role::User, "hello universe".into());

    let hits = store.search_in_session(&s1.id, "hello");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, s1.id);
    assert!(hits[0].snippet.contains("<mark>"));
}

#[test]
fn search_handles_fts5_special_chars() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "file at /usr/local/bin/test*".into());

    // Should not panic or error on special characters.
    let hits = store.search_messages("test*", 10);
    assert_eq!(hits.len(), 1);
}

#[test]
fn search_finds_tool_call_arguments() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    // A tool-call-only assistant row: empty text body, args live in tool_calls.
    let msg = store.add_message(&s.id, Role::Assistant, String::new());
    store.attach_tool_calls(
        &msg.id,
        &s.id,
        vec![ToolCall {
            id: "call_1".into(),
            name: "bash".into(),
            arguments: r#"{"command":"git rebase --interactive HEAD~3"}"#.into(),
        }],
    );

    // A term that appears only inside the tool-call arguments must be found by
    // both the cross-session and single-session search, with a `<mark>` snippet.
    let hits = store.search_messages("rebase", 10);
    assert_eq!(hits.len(), 1, "tool-call args should be searchable");
    assert_eq!(hits[0].message_id, msg.id);
    assert!(hits[0].snippet.contains("<mark>"));

    let scoped = store.search_in_session(&s.id, "rebase");
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].message_id, msg.id);
    assert!(scoped[0].snippet.contains("<mark>"));
}

// -- cross-process SQLITE_BUSY regression guard (#1080) -----------------
//
// The CLI now shares the GUI's db file, so two processes can hold open
// connections to the same store simultaneously. WAL allows concurrent readers
// but only one writer at a time; without a busy_timeout, the loser's write
// returns SQLITE_BUSY immediately, and `insert_session` ends in
// `.expect("insert session")` — panicking the losing process. This was
// unreachable when the CLI ran an ephemeral in-memory store; it is now the
// default path (`ff chat` with the desktop app open).
//
// `from_conn` pins `conn.busy_timeout(5s)` explicitly so the contract is
// self-documenting and independent of rusqlite's default (rusqlite 0.32 ships
// a 5s default today, but relying on that undocumented default is fragile).
// This test reproduces the contention deterministically by holding the WAL
// write lock on a raw connection (`BEGIN IMMEDIATE`) while the store writes,
// and asserts the store's write waits (not panics). It guards the behavior:
// if a future rusqlite drops its default AND our explicit call were removed,
// this test would panic.

#[test]
fn busy_timeout_lets_concurrent_writer_serialize() {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");

    // Open the store first (no contention yet) so its connection's busy_timeout
    // is in place and its schema migration completes before the lock is held.
    let store = SessionStore::open(&path).unwrap();

    // A second connection acquires the WAL write lock (`BEGIN IMMEDIATE`) and
    // holds it briefly, simulating a concurrent writer (the desktop GUI, or a
    // second CLI process) mid-write.
    let (ready_snd, ready_rcv) = mpsc::channel();
    let blocker_path = path.clone();
    let blocker = thread::spawn(move || {
        let conn = rusqlite::Connection::open(&blocker_path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE;").unwrap();
        ready_snd.send(()).unwrap(); // write lock now held
        thread::sleep(Duration::from_millis(200));
        conn.execute_batch("ROLLBACK;").unwrap();
    });

    ready_rcv.recv().unwrap(); // wait until the blocker holds the write lock

    // The store's create_session does an INSERT, which needs the write lock the
    // blocker holds. Without a busy_timeout this returns SQLITE_BUSY and panics
    // inside `insert_session` (`.expect("insert session")`). With the timeout
    // it waits for the blocker's ROLLBACK (~200ms), then succeeds.
    let session = store.create_session(Some("hello".into()));

    blocker.join().unwrap();
    assert!(store.get_session(&session.id).is_some());
    assert_eq!(session.goal.as_deref(), Some("hello"));
}
