use super::*;

/// A deferred stub with a controllable name/description/schema.
struct Deferred {
    name: &'static str,
    desc: &'static str,
    schema: Value,
}

#[async_trait]
impl Tool for Deferred {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        self.desc
    }
    fn parameters(&self) -> Value {
        self.schema.clone()
    }
    fn defer(&self) -> bool {
        true
    }
    async fn run(&self, _args: Value, _root: &Path) -> ToolOutcome {
        ToolOutcome::ok("ran")
    }
}

/// A resident stub — must never appear in search results.
struct Resident;

#[async_trait]
impl Tool for Resident {
    fn name(&self) -> &str {
        "resident_ticket_tool"
    }
    fn description(&self) -> &str {
        "Create a ticket. Resident, not deferred."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn run(&self, _args: Value, _root: &Path) -> ToolOutcome {
        ToolOutcome::ok("ran")
    }
}

fn deferred(name: &'static str, desc: &'static str, schema: Value) -> Box<dyn Tool> {
    Box::new(Deferred { name, desc, schema })
}

fn fixture() -> ToolSearchTool {
    let mut reg = ToolRegistry::default();
    reg.register(deferred(
        "ticketing_write",
        "Create and update tickets in the internal ticketing system.",
        json!({"type": "object", "properties": {
            "action": {"type": "string", "enum": ["create-ticket", "add-comment"]}
        }}),
    ));
    reg.register(deferred(
        "pipeline_health",
        "Retrieve status and health metrics for deployment pipelines.",
        json!({"type": "object", "properties": {"pipelineNames": {"type": "array"}}}),
    ));
    reg.register(deferred(
        "notes_vault",
        "Read and append to personal notes stored in a vault.",
        json!({"type": "object", "properties": {}}),
    ));
    reg.register(Box::new(Resident));
    ToolSearchTool::new(
        Arc::new(ToolSearchState::new()),
        ToolSearchIndex::from_registry(&reg),
    )
}

async fn search(tool: &ToolSearchTool, session: &str, args: Value) -> String {
    tool.run_with_session(args, Path::new("."), session)
        .await
        .content
}

#[test]
fn deferred_names_only_lists_opted_in_tools() {
    let mut reg = ToolRegistry::default();
    reg.register(deferred("a", "x", json!({})));
    reg.register(Box::new(Resident));
    let names = reg.deferred_tool_names();
    assert_eq!(names.len(), 1);
    assert!(names.contains("a"));
    assert!(!names.contains("resident_ticket_tool"));
}

#[tokio::test]
async fn matches_on_description_words() {
    let t = fixture();
    let out = search(&t, "s1", json!({"query": "file a ticket"})).await;
    assert!(out.contains("ticketing_write"), "got: {out}");
    assert!(!out.contains("notes_vault"), "unrelated hit: {out}");
}

#[tokio::test]
async fn matches_an_action_enum_value_from_the_schema() {
    // The recall case that matters for polymorphic tools: the capability lives in
    // the `action` enum, not the prose.
    let t = fixture();
    let out = search(&t, "s1", json!({"query": "add-comment"})).await;
    assert!(out.contains("ticketing_write"), "got: {out}");
}

#[tokio::test]
async fn never_returns_a_resident_tool() {
    let t = fixture();
    // "ticket" appears in the resident tool's description too.
    let out = search(&t, "s1", json!({"query": "ticket"})).await;
    assert!(!out.contains("resident_ticket_tool"), "got: {out}");
}

#[tokio::test]
async fn hits_are_capped_at_max_hits() {
    let mut reg = ToolRegistry::default();
    for i in 0..12 {
        // Leak a per-iteration name so the corpus has >MAX_HITS matching tools.
        let name: &'static str = Box::leak(format!("shared_tool_{i}").into_boxed_str());
        reg.register(deferred(name, "shared capability for searching", json!({})));
    }
    let t = ToolSearchTool::new(
        Arc::new(ToolSearchState::new()),
        ToolSearchIndex::from_registry(&reg),
    );
    let out = search(&t, "s1", json!({"query": "shared", "limit": 50})).await;
    assert_eq!(
        out.matches("- `shared_tool_").count(),
        MAX_HITS,
        "got: {out}"
    );
}

#[tokio::test]
async fn admits_hits_into_the_calling_session_only() {
    let state = Arc::new(ToolSearchState::new());
    let mut reg = ToolRegistry::default();
    reg.register(deferred(
        "ticketing_write",
        "Create and update tickets.",
        json!({}),
    ));
    let t = ToolSearchTool::new(state.clone(), ToolSearchIndex::from_registry(&reg));

    assert!(state.is_empty("s1"));
    search(&t, "s1", json!({"query": "tickets"})).await;

    assert!(state.admitted("s1").contains("ticketing_write"));
    assert!(
        state.admitted("s2").is_empty(),
        "another session must not be widened"
    );
    assert!(state.is_empty("s2"));
}

#[tokio::test]
async fn admission_is_cumulative_and_idempotent() {
    let state = Arc::new(ToolSearchState::new());
    let mut reg = ToolRegistry::default();
    reg.register(deferred("ticketing_write", "Create tickets.", json!({})));
    reg.register(deferred("pipeline_health", "Pipeline metrics.", json!({})));
    let t = ToolSearchTool::new(state.clone(), ToolSearchIndex::from_registry(&reg));

    search(&t, "s1", json!({"query": "tickets"})).await;
    search(&t, "s1", json!({"query": "tickets"})).await;
    search(&t, "s1", json!({"query": "pipeline"})).await;

    let admitted = state.admitted("s1");
    assert_eq!(admitted.len(), 2, "got: {admitted:?}");
}

#[tokio::test]
async fn no_match_is_a_success_not_an_error() {
    let t = fixture();
    let outcome = t
        .run_with_session(json!({"query": "zzzz nonexistent"}), Path::new("."), "s1")
        .await;
    assert!(outcome.success, "a miss must not read as a failure");
    assert!(outcome.content.contains("No tools matched"));
    // The message must send the model back for another attempt rather than
    // settling for what it already has — see
    // `retrieval_tests::the_empty_result_message_pushes_the_model_to_retry`.
    assert!(outcome.content.contains("Search again"));
}

#[tokio::test]
async fn missing_query_is_an_error() {
    let t = fixture();
    let outcome = t.run_with_session(json!({}), Path::new("."), "s1").await;
    assert!(!outcome.success);
}

#[test]
fn name_match_outranks_description_match() {
    let terms = vec!["pipeline".to_string()];
    let by_name = score_text("pipeline_health", "unrelated prose", "", &terms);
    let by_desc = score_text("other_tool", "queries a pipeline", "", &terms);
    assert!(by_name > by_desc, "{by_name} !> {by_desc}");
}

#[test]
fn description_match_outranks_schema_match() {
    let terms = vec!["create".to_string()];
    let by_desc = score_text("t1", "create a thing", "", &terms);
    let by_schema = score_text(
        "t2",
        "unrelated",
        &schema_keywords(&json!({"properties": {"action": {"enum": ["create"]}}})),
        &terms,
    );
    assert!(by_desc > by_schema, "{by_desc} !> {by_schema}");
    assert!(by_schema > 0, "schema text must still be searchable");
}

#[test]
fn multi_term_queries_concentrate_on_the_best_tool() {
    let terms: Vec<String> = "deployment pipeline status"
        .split(' ')
        .map(str::to_string)
        .collect();
    let strong = score_text(
        "pipeline_health",
        "retrieve status and health metrics for deployment pipelines.",
        "",
        &terms,
    );
    let weak = score_text("notes_vault", "read personal notes.", "", &terms);
    assert!(strong > weak, "{strong} !> {weak}");
}

#[test]
fn empty_query_matches_nothing() {
    assert_eq!(score_text("anything", "anything", "", &[]), 0);
}

#[test]
fn schema_keyword_extraction_walks_nested_enums() {
    let kw = schema_keywords(&json!({
        "type": "object",
        "properties": {
            "outer": {"type": "object", "properties": {"inner_field": {"type": "string"}}},
            "mode": {"enum": ["fast", "slow"]}
        }
    }));
    assert!(kw.contains("inner_field"), "got: {kw}");
    assert!(kw.contains("fast") && kw.contains("slow"), "got: {kw}");
}

#[test]
fn schema_keyword_extraction_is_depth_bounded() {
    // Guard against a pathological deeply-nested schema from a third-party server.
    let mut v = json!({"properties": {"leaf": {"type": "string"}}});
    for _ in 0..50 {
        v = json!({"properties": {"nest": v}});
    }
    let kw = schema_keywords(&v);
    assert!(
        !kw.contains("leaf"),
        "depth cap should stop before the leaf"
    );
}

#[test]
fn index_snapshots_only_deferred_tools() {
    let mut reg = ToolRegistry::default();
    reg.register(deferred("a", "x", json!({})));
    reg.register(deferred("b", "y", json!({})));
    reg.register(Box::new(Resident));
    let idx = ToolSearchIndex::from_registry(&reg);
    assert_eq!(idx.len(), 2, "residents must stay out of the corpus");
    assert!(!idx.is_empty());
}

#[test]
fn an_empty_index_is_searchable_and_yields_nothing() {
    let reg = ToolRegistry::default();
    let idx = ToolSearchIndex::from_registry(&reg);
    assert!(idx.is_empty());
    let t = ToolSearchTool::new(Arc::new(ToolSearchState::new()), idx);
    assert!(t.search("anything", 5).is_empty());
}

#[tokio::test]
async fn the_admitted_set_only_ever_grows() {
    // The turn loop's cheap guard (`unlocked.len() > appended.len()`) is only sound
    // because admissions are monotonic and never revoked — otherwise a shrink would
    // leave already-appended schemas advertising tools no longer in the set.
    let state = Arc::new(ToolSearchState::new());
    let mut reg = ToolRegistry::default();
    reg.register(deferred("alpha", "alpha capability", json!({})));
    reg.register(deferred("beta", "beta capability", json!({})));
    let t = ToolSearchTool::new(state.clone(), ToolSearchIndex::from_registry(&reg));

    let mut seen = 0usize;
    for q in ["alpha", "beta", "alpha", "capability"] {
        search(&t, "s1", json!({"query": q})).await;
        let n = state.admitted("s1").len();
        assert!(n >= seen, "admissions shrank from {seen} to {n}");
        seen = n;
    }
    assert_eq!(seen, 2);
}

/// A registry holding only `tool_search`, to exercise the two visibility
/// predicates that gate advertisement in restricted phenotypes.
fn registry_with_tool_search() -> ToolRegistry {
    let mut reg = ToolRegistry::default();
    let index = ToolSearchIndex::from_registry(&reg);
    reg.register(Box::new(ToolSearchTool::new(
        Arc::new(ToolSearchState::new()),
        index,
    )));
    reg
}

#[test]
fn tool_search_survives_the_plan_capability_filter() {
    // Plan advertises only tools whose *floor* is ReadOnly. `tool_search` is the
    // sole gateway to the deferred registry, so losing it in Plan would seal off
    // every deferred tool with no way to unlock them.
    let reg = registry_with_tool_search();
    assert!(
        reg.readonly_capable_names().contains("tool_search"),
        "tool_search must stay visible in Plan"
    );
    // Advertised *and* invocable: a ReadOnly ceiling with a Write per-call safety
    // would pass the filter, then be rejected by the approver.
    assert_eq!(reg.safety("tool_search", &json!({})), Safety::ReadOnly);
}

#[test]
fn tool_search_survives_the_local_only_filter() {
    // A LocalOnly phenotype keeps only `!reaches_network()` tools; the trait
    // default is a fail-safe `true`, which would drop `tool_search`.
    let reg = registry_with_tool_search();
    assert!(
        reg.local_tool_names().contains("tool_search"),
        "tool_search must stay visible under LocalOnly"
    );
}

#[test]
fn first_sentence_truncates_on_a_char_boundary() {
    // Descriptions are arbitrary third-party MCP metadata. A multi-byte codepoint
    // straddling the 160-byte cap used to panic on a non-boundary slice.
    let desc = "毫".repeat(200);
    let out = first_sentence(&desc);
    assert!(out.ends_with('…'), "expected truncation marker: {out}");
    assert!(out.len() <= 164, "cap overshot: {} bytes", out.len());
    assert!(out.trim_end_matches('…').chars().all(|c| c == '毫'));

    // A boundary landing exactly mid-codepoint: 'é' is 2 bytes, so byte 160 falls
    // inside one char.
    let mixed = format!("{}{}", "a".repeat(159), "é".repeat(20));
    let out = first_sentence(&mixed);
    assert!(out.starts_with(&"a".repeat(159)), "got: {out}");

    // Short input is returned whole, with no marker.
    assert_eq!(first_sentence("Short one."), "Short one.");
}

/// A failing embedder must produce *byte-identical* results to no embedder at all.
///
/// The weaker assertion — that results are merely non-empty — would pass even if
/// the semantic path silently reordered or dropped candidates on failure. Since
/// embeddings are opt-in, "embedder absent or broken" is the common case, and it
/// has to be indistinguishable from Phase 2A rather than approximately equal to it.
#[tokio::test]
async fn a_broken_embedder_yields_exactly_the_lexical_ranking() {
    /// Fully dead: neither corpus nor query embeds succeed.
    struct Dead;
    impl ff_memory::Embedder for Dead {
        fn embed_query(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(None)
        }
        fn embed_chunk(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(None)
        }
    }

    /// The nastier case: the corpus warms, then query embeds start failing (the
    /// server died mid-session, or rate-limited just this call). A half-warm cache
    /// must not be allowed to rank on its own.
    struct QueryOnlyFailure;
    impl ff_memory::Embedder for QueryOnlyFailure {
        fn embed_query(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(None)
        }
        fn embed_chunk(&self, text: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(Some(vec![text.len() as f32, 1.0]))
        }
    }

    fn boom() -> ff_memory::MemoryError {
        ff_memory::MemoryError::Io {
            path: std::path::PathBuf::from("embed"),
            source: std::io::Error::other("boom"),
        }
    }

    /// An embedder that errors rather than returning `None`.
    struct Erroring;
    impl ff_memory::Embedder for Erroring {
        fn embed_query(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Err(boom())
        }
        fn embed_chunk(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Err(boom())
        }
    }

    let queries = [
        "deploy",
        "search code",
        "run the tests",
        "read a file",
        "roll back a bad release",
    ];

    for q in queries {
        let baseline = search(&fixture(), "s0", json!({ "query": q })).await;

        for (label, tool) in [
            ("dead", fixture().with_embedder(Arc::new(Dead), "m")),
            (
                "query-only failure",
                fixture().with_embedder(Arc::new(QueryOnlyFailure), "m"),
            ),
            ("erroring", fixture().with_embedder(Arc::new(Erroring), "m")),
        ] {
            let got = search(&tool, "s1", json!({ "query": q })).await;
            assert_eq!(
                baseline, got,
                "a {label} embedder must not perturb the lexical ranking for {q:?}"
            );
        }
    }
}

/// The interactive timeout is short enough that a hung server cannot stall a turn.
///
/// Pinned as a bound rather than an exact value: the number may be tuned, but a
/// regression back to the indexing-path patience would be a real defect, since the
/// embed sits on the model's critical path.
#[test]
fn interactive_embeds_give_up_quickly() {
    assert!(
        ff_memory::INTERACTIVE_EMBED_TIMEOUT <= std::time::Duration::from_secs(5),
        "an interactive embed must not out-wait the user's patience, got {:?}",
        ff_memory::INTERACTIVE_EMBED_TIMEOUT
    );
}

/// A corpus that fails to embed must abandon the semantic path, not rank on a
/// partial cache.
///
/// The dangerous shape is a *partially* warm corpus: if warming half the tools
/// still enabled the vector path, those few would be the only semantic candidates
/// and would crowd out better lexical hits — a ranking decided by whichever embeds
/// happened to succeed. Asserted against the semantic path directly rather than
/// through the rendered output, because with a small corpus fusion can absorb the
/// difference and hide the defect.
#[tokio::test]
async fn a_corpus_that_will_not_embed_abandons_the_semantic_path() {
    /// Embeds exactly one tool, then refuses. Mimics a server dying mid-warm.
    struct OneThenDead(std::sync::atomic::AtomicUsize);
    impl ff_memory::Embedder for OneThenDead {
        fn embed_query(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0, 0.0]))
        }
        fn embed_chunk(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Ok(Some(vec![1.0, 0.0]))
            } else {
                Ok(None)
            }
        }
    }

    let partial = fixture().with_embedder(
        Arc::new(OneThenDead(std::sync::atomic::AtomicUsize::new(0))),
        "m",
    );

    let ranking = partial.semantic_ranking("notes").await;

    assert!(
        ranking.is_none(),
        "one embedded tool must not become the whole semantic candidate set, got {ranking:?}"
    );
}
