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
    fixture_sharing(Arc::new(ToolSearchState::new()))
}

/// A fixture over a caller-supplied `ToolSearchState`, so a test can build several
/// tools that share it. That is what production does: `build_tool_registry` clones
/// one long-lived `Arc<ToolSearchState>` into a fresh `ToolSearchTool` on every
/// turn, so state that must outlive a turn has to live on the *state*, not on the
/// tool.
fn fixture_sharing(state: Arc<ToolSearchState>) -> ToolSearchTool {
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
    ToolSearchTool::new(state, ToolSearchIndex::from_registry(&reg))
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
    assert!(t.search_fused("anything", 5, None).is_empty());
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

/// A blip during the first warm must not disable semantic recall for good.
///
/// The sibling test above pins "a partial cache does not rank". The trap is
/// implementing that as *permanent* surrender: gate the warm on `is_empty()` and a
/// single embed failure leaves the cache non-empty but short, so it is never warmed
/// again and every later search silently returns `None` — Phase 2B degrades to
/// BM25F for the rest of the process because of one dropped connection. Nothing
/// surfaces; the user has a healthy embedder and no recall.
///
/// Two calls on the *same* tool, because the bug lives in the state carried
/// between them. The first-call assertion is the sibling's contract; the second is
/// this one's.
#[tokio::test]
async fn a_transient_warm_failure_is_retried_on_the_next_search() {
    /// Fails every chunk on the first warm pass, then embeds normally. Mimics a
    /// server that was briefly unreachable.
    struct BlipThenHealthy {
        calls: std::sync::atomic::AtomicUsize,
        corpus: usize,
    }
    impl ff_memory::Embedder for BlipThenHealthy {
        fn embed_query(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0, 0.0]))
        }
        fn embed_chunk(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            // One tool lands, the rest of the first pass fails: a *partial* cache,
            // which is the state that used to be terminal. Later passes succeed.
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 || n >= self.corpus {
                Ok(Some(vec![1.0, 0.0]))
            } else {
                Ok(None)
            }
        }
    }

    let corpus = fixture().index.tools.len();
    let tool = fixture().with_embedder(
        Arc::new(BlipThenHealthy {
            calls: std::sync::atomic::AtomicUsize::new(0),
            corpus,
        }),
        "m",
    );

    assert!(
        tool.semantic_ranking("notes").await.is_none(),
        "a partial cache must not rank"
    );

    let recovered = tool.semantic_ranking("notes").await;

    assert!(
        recovered.is_some(),
        "the second search must re-warm the corpus and recover the semantic path; \
         got None, so a transient failure disabled semantic recall permanently"
    );
    assert_eq!(
        recovered.as_ref().map(Vec::len),
        Some(corpus),
        "a recovered ranking must cover the whole corpus"
    );
}

/// The warmed corpus must survive a turn.
///
/// `build_tool_registry` runs on *every* turn — `ask`, each `run_once` iteration of
/// a goal loop, each scheduled `fire` — and each run clones the shared
/// `Arc<ToolSearchState>` into a brand-new `ToolSearchTool`. Anything cached on the
/// tool is therefore thrown away between turns. When the corpus vectors lived
/// there, embedding the whole corpus was not a one-off warm cost but a per-turn tax
/// on the model's critical path, re-paid on every iteration.
///
/// Counts embed calls across two tools sharing one state: the second must ride the
/// first one's vectors and spend nothing.
#[tokio::test]
async fn the_warmed_corpus_outlives_a_registry_rebuild() {
    struct Counting(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl ff_memory::Embedder for Counting {
        fn embed_query(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0, 0.0]))
        }
        fn embed_chunk(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(vec![1.0, 0.0]))
        }
    }

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = Arc::new(ToolSearchState::new());
    let embedder = |c: &std::sync::Arc<std::sync::atomic::AtomicUsize>| {
        Arc::new(Counting(std::sync::Arc::clone(c))) as Arc<dyn ff_memory::Embedder>
    };

    // Turn one: a cold corpus, so this one pays to warm it.
    let first = fixture_sharing(Arc::clone(&state)).with_embedder(embedder(&calls), "m");
    assert!(first.semantic_ranking("notes").await.is_some());
    let warmed = calls.load(std::sync::atomic::Ordering::SeqCst);
    assert!(warmed > 0, "the first turn must warm the corpus");

    // Turn two: the registry is rebuilt, the state is the same.
    let second = fixture_sharing(Arc::clone(&state)).with_embedder(embedder(&calls), "m");
    assert!(second.semantic_ranking("notes").await.is_some());

    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        warmed,
        "the corpus was re-embedded after a registry rebuild: warming {warmed} chunks is a \
         per-turn cost, not a one-off"
    );
}

/// Switching embedding models must drop the old vectors, not just relabel them.
///
/// The cache outlives any one tool now, so a model switch is handled in place by
/// `retarget`. Relabelling without clearing is the subtle failure: `len()` counts
/// entries regardless of model while `get` filters by it, so a stale-but-full cache
/// reports a complete corpus whose every lookup then misses — the warm gate is
/// satisfied, no re-embed happens, and semantic ranking silently returns nothing
/// for the rest of the process.
#[tokio::test]
async fn switching_models_reembeds_the_corpus() {
    struct Tagged(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl ff_memory::Embedder for Tagged {
        fn embed_query(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0, 0.0]))
        }
        fn embed_chunk(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(vec![1.0, 0.0]))
        }
    }

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = Arc::new(ToolSearchState::new());
    let make = |model: &str| {
        fixture_sharing(Arc::clone(&state))
            .with_embedder(Arc::new(Tagged(std::sync::Arc::clone(&calls))), model)
    };

    assert!(make("model-a").semantic_ranking("notes").await.is_some());
    let after_a = calls.load(std::sync::atomic::Ordering::SeqCst);
    assert!(after_a > 0, "the first model must warm the corpus");

    // A different model: the cached vectors are not comparable and must be redone.
    let ranking = make("model-b").semantic_ranking("notes").await;

    assert!(
        ranking.is_some(),
        "a model switch must leave a usable corpus; got None, so the cache reported \
         itself full while every lookup missed"
    );
    assert!(
        calls.load(std::sync::atomic::Ordering::SeqCst) > after_a,
        "switching models must re-embed the corpus, but no new embed calls were made"
    );
}

/// The retry budget must survive a registry rebuild too.
///
/// Sibling `a_hopeless_corpus_stops_retrying_after_the_budget` drives one tool, so
/// it cannot see a budget that resets per turn — and production never reuses a tool
/// across turns. A per-turn counter turns "give up on a corpus that cannot embed"
/// into "retry three times, every single turn, forever": the cap reads as a
/// safeguard while costing a full doomed warm on the model's critical path each
/// iteration. Rebuilds the tool between searches, as `build_tool_registry` does.
#[tokio::test]
async fn the_retry_budget_outlives_a_registry_rebuild() {
    struct NeverEmbeds(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl ff_memory::Embedder for NeverEmbeds {
        fn embed_query(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0, 0.0]))
        }
        fn embed_chunk(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(None)
        }
    }

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = Arc::new(ToolSearchState::new());
    let turns = super::MAX_WARM_ATTEMPTS + 3;

    for _ in 0..turns {
        let tool = fixture_sharing(Arc::clone(&state))
            .with_embedder(Arc::new(NeverEmbeds(std::sync::Arc::clone(&calls))), "m");
        assert!(tool.semantic_ranking("notes").await.is_none());
    }

    let spent = calls.load(std::sync::atomic::Ordering::SeqCst);
    let corpus = fixture().index.tools.len();
    assert!(
        spent <= corpus * super::MAX_WARM_ATTEMPTS,
        "the warm budget reset with the registry: {spent} embed calls over {turns} turns \
         (corpus {corpus}, budget {})",
        super::MAX_WARM_ATTEMPTS
    );
}

/// The retry budget is finite: a corpus that can *never* embed must stop paying for
/// a doomed warm on every search.
///
/// Without a cap, the fix above turns a permanent embedder failure into a permanent
/// tax — every `tool_search` would re-attempt the full corpus on the model's
/// critical path, forever. Counts embed calls across more searches than the budget
/// allows and asserts they stop.
#[tokio::test]
async fn a_hopeless_corpus_stops_retrying_after_the_budget() {
    struct NeverEmbeds(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl ff_memory::Embedder for NeverEmbeds {
        fn embed_query(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0, 0.0]))
        }
        fn embed_chunk(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(None)
        }
    }

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool = fixture().with_embedder(Arc::new(NeverEmbeds(std::sync::Arc::clone(&calls))), "m");

    for _ in 0..(super::MAX_WARM_ATTEMPTS + 3) {
        assert!(tool.semantic_ranking("notes").await.is_none());
    }

    let spent = calls.load(std::sync::atomic::Ordering::SeqCst);
    let corpus = fixture().index.tools.len();
    assert!(
        spent <= corpus * super::MAX_WARM_ATTEMPTS,
        "a hopeless corpus must stop warming after {} attempts, but spent {spent} embed calls \
         over {} searches (corpus {corpus})",
        super::MAX_WARM_ATTEMPTS,
        super::MAX_WARM_ATTEMPTS + 3
    );
}
