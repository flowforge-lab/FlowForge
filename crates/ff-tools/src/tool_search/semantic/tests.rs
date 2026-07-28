use super::*;

fn ranked<'a>(names: &[&'a str]) -> Ranked<'a> {
    names.to_vec()
}

/// Fusing with an empty semantic list must preserve the lexical order exactly.
///
/// This is the property the whole fallback story rests on: embeddings are opt-in,
/// so "no embedder" is the common case, and it has to be indistinguishable from
/// Phase 2A rather than merely close to it.
#[test]
fn fusing_with_an_absent_path_is_the_identity() {
    let lexical = ranked(&["deploy_read", "pipeline_health", "code_search"]);

    let fused = rrf_fuse(&lexical, &ranked(&[]));

    assert_eq!(fused, vec!["deploy_read", "pipeline_health", "code_search"]);
}

/// Symmetrically: with no lexical hits at all, the semantic order passes through.
/// This is the vocabulary-gap case — BM25F scores zero, and recall comes entirely
/// from the vector path.
#[test]
fn a_semantic_only_query_still_returns_candidates() {
    let semantic = ranked(&["oncall_read", "tracker_write"]);

    let fused = rrf_fuse(&ranked(&[]), &semantic);

    assert_eq!(fused, vec!["oncall_read", "tracker_write"]);
}

/// A tool both paths rank modestly should beat one that only a single path ranks
/// first. This is the point of fusing rather than concatenating.
#[test]
fn agreement_across_paths_outranks_a_single_strong_hit() {
    let lexical = ranked(&["only_lexical", "agreed"]);
    let semantic = ranked(&["only_semantic", "agreed"]);

    let fused = rrf_fuse(&lexical, &semantic);

    assert_eq!(
        fused.first().map(String::as_str),
        Some("agreed"),
        "a tool found by both paths must win: {fused:?}"
    );
}

#[test]
fn fusion_order_is_deterministic_for_equal_scores() {
    let a = rrf_fuse(
        &ranked(&["b_tool", "a_tool"]),
        &ranked(&["a_tool", "b_tool"]),
    );
    let b = rrf_fuse(
        &ranked(&["a_tool", "b_tool"]),
        &ranked(&["b_tool", "a_tool"]),
    );

    assert_eq!(
        a, b,
        "equal scores must break ties by name, not by input order"
    );
}

/// A vector cached under one model must not be served for another.
///
/// The dangerous case is same-dimension mixing: it produces cosine noise with no
/// error and no failing test, so the guard has to be the key itself.
#[test]
fn vectors_are_not_shared_across_models() {
    let mut v = CorpusVectors::new("nomic-embed-text");
    v.insert("deploy_read read deployments", vec![1.0, 0.0, 0.0]);

    assert!(v
        .get("nomic-embed-text", "deploy_read read deployments")
        .is_some());
    assert!(
        v.get("embeddinggemma", "deploy_read read deployments")
            .is_none(),
        "a foreign model's vector must be invisible, not merely deprioritised"
    );
}

/// Editing one tool's text must not invalidate the others.
#[test]
fn content_keying_invalidates_only_the_changed_entry() {
    let mut v = CorpusVectors::new("m");
    v.insert("first text", vec![1.0, 0.0]);
    v.insert("second text", vec![0.0, 1.0]);

    assert!(v.get("m", "first text").is_some());
    assert!(v.get("m", "second text").is_some());
    assert!(
        v.get("m", "first text edited").is_none(),
        "changed text must miss the cache"
    );
}

#[test]
fn semantic_ranking_orders_by_similarity() {
    let mut v = CorpusVectors::new("m");
    let texts = vec![
        ("near", "near text".to_string()),
        ("far", "far text".to_string()),
    ];
    v.insert("near text", vec![1.0, 0.0]);
    v.insert("far text", vec![0.0, 1.0]);

    let ranked = semantic_ranking(&[0.9, 0.1], &texts, &v, "m");

    assert_eq!(ranked, vec!["near", "far"]);
}

/// An entry with no cached vector is skipped, not scored as distant.
#[test]
fn a_partially_warm_cache_yields_a_smaller_set_not_a_wrong_one() {
    let mut v = CorpusVectors::new("m");
    let texts = vec![
        ("cached", "cached text".to_string()),
        ("uncached", "uncached text".to_string()),
    ];
    v.insert("cached text", vec![1.0, 0.0]);

    let ranked = semantic_ranking(&[1.0, 0.0], &texts, &v, "m");

    assert_eq!(
        ranked,
        vec!["cached"],
        "an unembedded tool must be absent from the vector path, not ranked badly"
    );
}

/// A dimension mismatch scores 0.0 and drops out, rather than comparing
/// incomparable vector spaces.
#[test]
fn a_dimension_mismatch_drops_the_entry() {
    let mut v = CorpusVectors::new("m");
    let texts = vec![("wrong_dims", "t".to_string())];
    v.insert("t", vec![1.0, 0.0, 0.0]);

    let ranked = semantic_ranking(&[1.0, 0.0], &texts, &v, "m");

    assert!(
        ranked.is_empty(),
        "a 3-dim cached vector must not be scored against a 2-dim query"
    );
}

/// Switching models must clear the cache rather than accumulate both.
#[test]
fn warming_under_a_new_model_discards_the_old_vectors() {
    struct Fixed(f32);
    impl Embedder for Fixed {
        fn embed_query(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(Some(vec![self.0, 0.0]))
        }
        fn embed_chunk(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(Some(vec![self.0, 0.0]))
        }
    }

    let texts = vec![("a", "a text".to_string())];
    let mut v = CorpusVectors::new("old-model");
    let e: Arc<dyn Embedder> = Arc::new(Fixed(1.0));
    warm(&e, "old-model", &texts, &mut v);
    assert_eq!(v.len(), 1);

    warm(&e, "new-model", &texts, &mut v);

    assert_eq!(
        v.len(),
        1,
        "a model change must reset the cache, not add a second generation"
    );
    assert!(v.get("old-model", "a text").is_none());
    assert!(v.get("new-model", "a text").is_some());
}

/// A failing embedder must leave the cache empty and not panic — the caller then
/// falls back to BM25F.
#[test]
fn warming_with_a_failing_embedder_leaves_the_cache_empty() {
    struct Broken;
    impl Embedder for Broken {
        fn embed_query(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(None)
        }
        fn embed_chunk(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(None)
        }
    }

    let texts = vec![("a", "a text".to_string())];
    let mut v = CorpusVectors::new("m");
    let e: Arc<dyn Embedder> = Arc::new(Broken);

    let added = warm(&e, "m", &texts, &mut v);

    assert_eq!(added, 0);
    assert!(v.is_empty(), "a dead embedder must not poison the cache");
}

/// Re-warming an already-cached corpus must not re-embed it.
#[test]
fn warming_is_idempotent() {
    struct Counting(std::sync::atomic::AtomicUsize);
    impl Embedder for Counting {
        fn embed_query(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            Ok(Some(vec![1.0]))
        }
        fn embed_chunk(&self, _: &str) -> ff_memory::Result<Option<Vec<f32>>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(vec![1.0]))
        }
    }

    let texts = vec![("a", "a text".to_string()), ("b", "b text".to_string())];
    let mut v = CorpusVectors::new("m");
    let counting = Arc::new(Counting(std::sync::atomic::AtomicUsize::new(0)));
    let e: Arc<dyn Embedder> = counting.clone();

    warm(&e, "m", &texts, &mut v);
    let after_first = counting.0.load(std::sync::atomic::Ordering::SeqCst);
    warm(&e, "m", &texts, &mut v);

    assert_eq!(after_first, 2);
    assert_eq!(
        counting.0.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "a warm cache must cost zero embed calls"
    );
}
