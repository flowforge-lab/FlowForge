use std::path::Path;

use super::*;

fn mem(root: &Path) -> Memory {
    Memory::new(root, MemoryConfig::default())
}

#[test]
fn insert_under_heading_creates_section_when_absent() {
    assert_eq!(
        insert_under_heading("", "## Identity", "L5 SDE"),
        "## Identity\nL5 SDE\n"
    );
    assert_eq!(
        insert_under_heading("## Patterns\nuses Python\n", "## Identity", "L5 SDE"),
        "## Patterns\nuses Python\n\n## Identity\nL5 SDE\n"
    );
}

#[test]
fn insert_under_heading_appends_to_existing_section() {
    let out = insert_under_heading("## Identity\nL5 SDE\n", "## Identity", "based in Austin");
    assert_eq!(out, "## Identity\nL5 SDE\nbased in Austin\n");
}

#[test]
fn insert_under_heading_inserts_before_next_sibling() {
    let content = "## Identity\nL5 SDE\n\n## Focus\nmaps work\n";
    let out = insert_under_heading(content, "## Identity", "based in Austin");
    assert_eq!(
        out,
        "## Identity\nL5 SDE\nbased in Austin\n\n## Focus\nmaps work\n"
    );
}

#[test]
fn write_curated_stratum_routes_to_heading() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    m.write_curated_stratum("L5 SDE on Maps", Stratum::Identity)
        .unwrap();
    m.write_curated_stratum("prefers Python", Stratum::Patterns)
        .unwrap();
    m.write_curated_stratum("based in Austin", Stratum::Identity)
        .unwrap();
    let curated = read_lenient(&m.curated_path());
    assert_eq!(
        curated,
        "## Identity\nL5 SDE on Maps\nbased in Austin\n\n## Patterns\nprefers Python\n"
    );
}

#[test]
fn get_rejects_path_traversal_and_absolute_escape() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    std::fs::create_dir_all(&root).unwrap();
    // A secret sibling outside the memory root.
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "TOP-SECRET").unwrap();
    let m = mem(&root);
    // Relative traversal out of the root must not read the sibling.
    assert_eq!(m.get(&root.join("../secret.txt"), None, None), "");
    // An absolute path outside the root is likewise rejected.
    assert_eq!(m.get(&secret, None, None), "");
}

#[test]
fn list_files_orders_curated_then_daily_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("MEMORY.md"), "# curated\nhello").unwrap();
    std::fs::create_dir_all(root.join("daily")).unwrap();
    std::fs::write(root.join("daily/2026-06-16.md"), "older").unwrap();
    std::fs::write(root.join("daily/2026-06-18.md"), "newer").unwrap();
    // A non-Markdown sibling and the derived index must be ignored.
    std::fs::write(root.join("index.db"), "binary").unwrap();
    std::fs::write(root.join("daily/notes.txt"), "x").unwrap();

    let files = mem(root).list_files();
    let names: Vec<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
    assert_eq!(
        names,
        vec!["MEMORY.md", "daily/2026-06-18.md", "daily/2026-06-16.md"]
    );
    assert_eq!(files[0].kind, MemoryFileKind::Curated);
    assert_eq!(files[1].kind, MemoryFileKind::Daily);
    assert!(files[0].size_bytes > 0);
    assert!(files.iter().all(|f| f.modified_ms >= 0));
}

#[test]
fn list_files_empty_when_nothing_recorded() {
    let dir = tempfile::tempdir().unwrap();
    assert!(mem(dir.path()).list_files().is_empty());
}

#[test]
fn read_file_round_trips_and_rejects_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    std::fs::create_dir_all(root.join("daily")).unwrap();
    std::fs::write(root.join("MEMORY.md"), "curated body").unwrap();
    std::fs::write(root.join("daily/2026-06-18.md"), "daily body").unwrap();
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "TOP-SECRET").unwrap();
    let m = mem(&root);

    assert_eq!(m.read_file("MEMORY.md").as_deref(), Some("curated body"));
    assert_eq!(
        m.read_file("daily/2026-06-18.md").as_deref(),
        Some("daily body")
    );
    // Missing-but-in-root reads as empty, never an error.
    assert_eq!(m.read_file("daily/2099-01-01.md").as_deref(), Some(""));
    // Traversal escapes are rejected outright.
    assert_eq!(m.read_file("../secret.txt"), None);
    assert_eq!(m.read_file("daily/../../secret.txt"), None);
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[test]
fn missing_root_yields_no_ambient_block() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(mem(dir.path()).ambient_block(), None);
}

#[test]
fn read_lenient_treats_missing_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_lenient(&dir.path().join("nope.md")), "");
}

#[test]
fn curated_only_block() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    write(&m.curated_path(), "User prefers Rust.\n");
    let block = m.ambient_block().unwrap();
    assert!(block.starts_with("## Memory\n"));
    assert!(block.contains("User prefers Rust."));
    assert!(!block.contains("Recent daily log"));
}

#[test]
fn daily_today_and_yesterday_included_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    let today = NaiveDate::from_ymd_opt(2026, 6, 17).unwrap();
    let yesterday = NaiveDate::from_ymd_opt(2026, 6, 16).unwrap();
    write(&m.daily_path(today), "shipped the rename");
    write(&m.daily_path(yesterday), "filed M5 epic");
    let block = m.ambient_block_for(today).unwrap();
    let y = block.find("filed M5 epic").unwrap();
    let t = block.find("shipped the rename").unwrap();
    assert!(y < t, "yesterday should precede today: {block}");
    assert!(block.contains("Yesterday (2026-06-16)"));
    assert!(block.contains("Today (2026-06-17)"));
}

#[test]
fn disabled_config_yields_nothing_even_with_files() {
    let dir = tempfile::tempdir().unwrap();
    let m = Memory::new(
        dir.path(),
        MemoryConfig {
            enabled: false,
            ..Default::default()
        },
    );
    write(&m.curated_path(), "should not appear");
    assert_eq!(m.ambient_block(), None);
}

#[test]
fn oversized_curated_is_truncated_with_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let m = Memory::new(
        dir.path(),
        MemoryConfig {
            enabled: true,
            injection_budget_bytes: 40,
            ..Default::default()
        },
    );
    let body = (0..20)
        .map(|i| format!("line {i} with some text"))
        .collect::<Vec<_>>()
        .join("\n");
    write(&m.curated_path(), &body);
    let block = m.ambient_block().unwrap();
    assert!(block.contains("memory truncated"));
    assert!(block.contains("memory_search"));
    assert!(block.contains("line 0"));
    assert!(!block.contains("line 19"));
}

#[test]
fn head_within_cuts_on_line_boundary() {
    let text = "aaaa\nbbbb\ncccc\ndddd";
    // budget lands inside the third line; keep through the second.
    assert_eq!(head_within(text, 12), "aaaa\nbbbb");
}

#[test]
fn head_within_returns_all_when_under_budget() {
    let text = "short";
    assert_eq!(head_within(text, 999), "short");
}

#[test]
fn chunk_markdown_splits_on_headings() {
    let md = "# Title\nintro line\n\n## Prefs\nlikes rust\nhates yaml\n\n## Decisions\nuse sqlite";
    let chunks = chunk_markdown(md, MemorySource::Curated, Path::new("MEMORY.md"));
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].heading.as_deref(), Some("Title"));
    assert_eq!(chunks[1].heading.as_deref(), Some("Prefs"));
    assert!(chunks[1].text.contains("likes rust"));
    assert!(chunks[1].text.contains("hates yaml"));
    assert_eq!(chunks[2].heading.as_deref(), Some("Decisions"));
    assert!(chunks[2].embedding.is_none());
}

#[test]
fn chunk_markdown_preamble_before_first_heading() {
    let md = "loose note\nanother\n# Heading\nbody";
    let chunks = chunk_markdown(md, MemorySource::Curated, Path::new("MEMORY.md"));
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].heading, None);
    assert_eq!(chunks[0].line_start, 1);
    assert_eq!(chunks[0].line_end, 2);
    assert!(chunks[0].text.contains("loose note"));
    assert_eq!(chunks[1].heading.as_deref(), Some("Heading"));
    assert_eq!(chunks[1].line_start, 3);
}

#[test]
fn chunk_markdown_empty_input_yields_no_chunks() {
    assert!(chunk_markdown("", MemorySource::Curated, Path::new("x.md")).is_empty());
    assert!(chunk_markdown("   \n  \n", MemorySource::Curated, Path::new("x.md")).is_empty());
}

#[test]
fn small_section_is_a_single_chunk_unchanged() {
    // A section well under the target stays one chunk with the heading-anchored
    // line span -> byte-identical to the pre-windowing behaviour.
    let md = "# h\nshort body line one\nshort body line two";
    let chunks = chunk_markdown(md, MemorySource::Curated, Path::new("x.md"));
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].line_start, 1);
    assert_eq!(chunks[0].line_end, 3);
    assert_eq!(chunks[0].text, md);
}

#[test]
fn oversized_section_splits_into_overlapping_windows_with_exact_line_spans() {
    // Build one heading section far larger than CHUNK_TARGET_BYTES.
    let mut md = String::from("# big\n");
    for i in 0..200 {
        md.push_str(&format!(
            "line {i:03} with enough text to add real bytes here\n"
        ));
    }
    let chunks = chunk_markdown(&md, MemorySource::Curated, Path::new("x.md"));
    assert!(chunks.len() > 1, "oversized section should window");
    // Every sub-chunk inherits the heading and stays within the target (each
    // window is allowed to overshoot only by its first line).
    for c in &chunks {
        assert_eq!(c.heading.as_deref(), Some("big"));
        assert!(c.line_start >= 1 && c.line_end >= c.line_start);
    }
    // Windows are contiguous-with-overlap: each starts at or before the
    // previous window's end (the carried-over context), and the last window
    // reaches the final line of the section.
    let total_lines = md.lines().count() as u32;
    for pair in chunks.windows(2) {
        assert!(pair[1].line_start <= pair[0].line_end + 1);
        assert!(pair[1].line_start > pair[0].line_start);
    }
    assert_eq!(chunks.last().unwrap().line_end, total_lines);
    // Sub-chunk text matches its reported line span exactly.
    let lines: Vec<&str> = md.lines().collect();
    for c in &chunks {
        let expected = lines[(c.line_start - 1) as usize..=(c.line_end - 1) as usize].join("\n");
        assert_eq!(c.text, expected);
    }
}

#[test]
fn window_line_ranges_covers_all_lines_and_advances() {
    let lines: Vec<&str> = vec!["aaaa"; 50];
    let ranges = window_line_ranges(&lines, 20, 6);
    assert!(ranges.len() > 1);
    assert_eq!(ranges.first().unwrap().0, 0);
    assert_eq!(ranges.last().unwrap().1, lines.len());
    for pair in ranges.windows(2) {
        assert!(pair[1].0 > pair[0].0, "must advance");
        assert!(pair[1].0 < pair[0].1, "must overlap");
    }
}

#[test]
fn memory_config_default_keeps_embeddings_off() {
    let cfg = MemoryConfig::default();
    assert!(cfg.enabled);
    assert_eq!(cfg.injection_budget_bytes, 4096);
    assert!(!cfg.embeddings.enabled);
    assert_eq!(cfg.embeddings.provider, EmbeddingProvider::Local);
}

// --- rewrite_curated tests (P1, #223) ---

#[test]
fn rewrite_curated_creates_file_from_scratch() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    m.rewrite_curated("# Fresh\nNew curated content\n").unwrap();
    let content = std::fs::read_to_string(m.curated_path()).unwrap();
    assert_eq!(content, "# Fresh\nNew curated content\n");
}

#[test]
fn rewrite_curated_replaces_existing_content_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    // Write initial content via append path
    m.write("old fact", WriteTarget::Curated).unwrap();
    assert!(std::fs::read_to_string(m.curated_path())
        .unwrap()
        .contains("old fact"));
    // Atomic rewrite replaces entirely
    m.rewrite_curated("# Consolidated\nnew fact only\n")
        .unwrap();
    let content = std::fs::read_to_string(m.curated_path()).unwrap();
    assert!(!content.contains("old fact"));
    assert!(content.contains("new fact only"));
}

#[test]
fn rewrite_curated_no_partial_write_on_empty() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    m.rewrite_curated("").unwrap();
    let content = std::fs::read_to_string(m.curated_path()).unwrap();
    assert_eq!(content, "");
}

#[test]
fn needs_consolidation_false_when_under_budget() {
    let dir = tempfile::tempdir().unwrap();
    let m = Memory::new(
        dir.path(),
        MemoryConfig {
            injection_budget_bytes: 1000,
            ..Default::default()
        },
    );
    // No file -> no consolidation needed
    assert!(!m.needs_consolidation());
    // Small file -> still no
    m.rewrite_curated("small").unwrap();
    assert!(!m.needs_consolidation());
}

#[test]
fn needs_consolidation_true_when_over_budget_with_hysteresis() {
    let dir = tempfile::tempdir().unwrap();
    let m = Memory::new(
        dir.path(),
        MemoryConfig {
            injection_budget_bytes: 100,
            ..Default::default()
        },
    );
    // Exactly at budget (100 bytes) -> no (hysteresis = 110%)
    m.rewrite_curated(&"x".repeat(100)).unwrap();
    assert!(!m.needs_consolidation());
    // At 110 bytes -> no (need to exceed 110)
    m.rewrite_curated(&"x".repeat(110)).unwrap();
    assert!(!m.needs_consolidation());
    // At 111 bytes -> yes
    m.rewrite_curated(&"x".repeat(111)).unwrap();
    assert!(m.needs_consolidation());
}

// --- Ambient dormant-skip (RFC 0007 §M6.1, #292) ---

use crate::index::{Fts5Index, ScoredChunk};

fn enabled_index() -> Fts5Index {
    Fts5Index::open_in_memory()
        .unwrap()
        .with_decay(DecayConfig {
            enabled: true,
            ..DecayConfig::default()
        })
}

fn disabled_index() -> Fts5Index {
    Fts5Index::open_in_memory()
        .unwrap()
        .with_decay(DecayConfig {
            enabled: false,
            ..DecayConfig::default()
        })
}

const DAY_MS: i64 = 86_400_000;
const T0: i64 = 1_700_000_000_000;

fn search_for(idx: &Fts5Index, q: &str) -> Vec<ScoredChunk> {
    idx.search(q, 10).unwrap()
}

#[test]
fn ambient_skips_dormant_curated_chunk_keeps_live() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    write(
        &m.curated_path(),
        "## Likes\nuser prefers rust\n\n## Dislikes\nuser dislikes verbose logs\n",
    );
    let idx = enabled_index();
    idx.reindex(&m.all_chunks()).unwrap();

    // Recall the Likes chunk once, long ago, so it decays dormant by `future`.
    let hits = search_for(&idx, "rust");
    assert_eq!(hits.len(), 1, "only the Likes chunk matches 'rust'");
    idx.reinforce_at(&hits, T0).unwrap();
    let future = T0 + 500 * DAY_MS;

    let today = NaiveDate::from_ymd_opt(2026, 6, 22).unwrap();
    let block = m.ambient_block_filtered_for(&idx, today, future).unwrap();

    // Dormant Likes chunk (heading + body) excised; the never-recalled
    // Dislikes chunk stays (no stats row -> weight 1.0 -> never dormant).
    assert!(
        !block.contains("user prefers rust"),
        "dormant body excised: {block}"
    );
    assert!(
        !block.contains("## Likes"),
        "dormant heading excised: {block}"
    );
    assert!(block.contains("## Dislikes"), "live heading kept: {block}");
    assert!(
        block.contains("user dislikes verbose logs"),
        "live body kept: {block}"
    );
}

#[test]
fn ambient_keeps_pinned_curated_chunk_even_when_decayed() {
    // A chunk recalled long ago WOULD decay dormant and be excised — but
    // pinning it holds effective weight at 1.0, so curated_filter (which reads
    // effective_stats) keeps it in the ambient block. Pinning thus retains a
    // fact in ambient injection as well as out of dormancy (RFC 0007 §7, #293).
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    write(
        &m.curated_path(),
        "## Likes\nuser prefers rust\n\n## Dislikes\nuser dislikes verbose logs\n",
    );
    let idx = enabled_index();
    idx.reindex(&m.all_chunks()).unwrap();

    let hits = search_for(&idx, "rust");
    idx.reinforce_at(&hits, T0).unwrap();
    let likes_key = crate::chunk_key(&hits[0].chunk);
    idx.set_chunk_pinned_at(&likes_key, true, T0).unwrap();
    let future = T0 + 500 * DAY_MS;

    let today = NaiveDate::from_ymd_opt(2026, 6, 22).unwrap();
    let block = m.ambient_block_filtered_for(&idx, today, future).unwrap();

    assert!(
        block.contains("user prefers rust"),
        "pinned chunk retained in ambient despite long decay: {block}"
    );
    assert!(block.contains("## Likes"), "pinned heading kept: {block}");
}

#[test]
fn ambient_filtered_byte_identical_when_decay_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    write(
        &m.curated_path(),
        "## Likes\nuser prefers rust\n\n## Dislikes\nuser dislikes verbose logs\n",
    );
    let today = NaiveDate::from_ymd_opt(2026, 6, 22).unwrap();
    write(&m.daily_path(today), "shipped the dormant skip");

    let idx = disabled_index();
    idx.reindex(&m.all_chunks()).unwrap();
    // Even after a recall, a disabled index never decays -> nothing dormant.
    idx.reinforce_at(&search_for(&idx, "rust"), T0).unwrap();
    let future = T0 + 500 * DAY_MS;

    assert_eq!(
        m.ambient_block_filtered_for(&idx, today, future),
        m.ambient_block_for(today),
        "decay-disabled filtered ambient must be byte-identical to unfiltered",
    );
}

#[test]
fn ambient_excision_keeps_surrounding_text_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    write(
        &m.curated_path(),
        "## Alpha\nfirst section keep me\n\n## Beta\nmiddle section rust drop\n\n## Gamma\nlast section keep me too\n",
    );
    let idx = enabled_index();
    idx.reindex(&m.all_chunks()).unwrap();
    idx.reinforce_at(&search_for(&idx, "rust"), T0).unwrap();
    let future = T0 + 500 * DAY_MS;

    let today = NaiveDate::from_ymd_opt(2026, 6, 22).unwrap();
    let block = m.ambient_block_filtered_for(&idx, today, future).unwrap();

    assert!(!block.contains("## Beta"));
    assert!(!block.contains("middle section rust drop"));
    // Surrounding sections survive verbatim, in order.
    assert!(block.contains("## Alpha\nfirst section keep me"));
    assert!(block.contains("## Gamma\nlast section keep me too"));
    let a = block.find("## Alpha").unwrap();
    let g = block.find("## Gamma").unwrap();
    assert!(a < g, "section order preserved: {block}");
}

#[test]
fn ambient_filter_leaves_daily_section_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    write(&m.curated_path(), "## Likes\nuser prefers rust\n");
    let today = NaiveDate::from_ymd_opt(2026, 6, 22).unwrap();
    write(&m.daily_path(today), "today I shipped dormancy");

    let idx = enabled_index();
    idx.reindex(&m.all_chunks()).unwrap();
    idx.reinforce_at(&search_for(&idx, "rust"), T0).unwrap();
    let future = T0 + 500 * DAY_MS;

    let block = m.ambient_block_filtered_for(&idx, today, future).unwrap();
    // Curated dormant chunk gone, daily log intact.
    assert!(!block.contains("user prefers rust"));
    assert!(block.contains("Recent daily log"));
    assert!(block.contains("today I shipped dormancy"));
}

#[test]
fn ambient_wake_via_recall_restores_dormant_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    // A never-recalled anchor keeps the block non-empty across both states.
    write(
        &m.curated_path(),
        "## Pinned\nanchor stays\n\n## Likes\nuser prefers rust\n",
    );
    let idx = enabled_index();
    idx.reindex(&m.all_chunks()).unwrap();

    idx.reinforce_at(&search_for(&idx, "rust"), T0).unwrap();
    let future = T0 + 500 * DAY_MS;
    let today = NaiveDate::from_ymd_opt(2026, 6, 22).unwrap();

    // Dormant at `future`.
    let before = m.ambient_block_filtered_for(&idx, today, future).unwrap();
    assert!(
        !before.contains("user prefers rust"),
        "dormant before recall: {before}"
    );

    // A recall at `future` reinforces the chunk back above threshold.
    idx.reinforce_at(&search_for(&idx, "rust"), future).unwrap();
    let after = m.ambient_block_filtered_for(&idx, today, future).unwrap();
    assert!(
        after.contains("user prefers rust"),
        "woken by recall: {after}"
    );
}
#[test]
fn keyed_ambient_returns_live_curated_keys_only() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    write(
        &m.curated_path(),
        "## Likes\nuser prefers rust\n\n## Dislikes\nuser dislikes verbose logs\n",
    );
    let today = NaiveDate::from_ymd_opt(2026, 6, 22).unwrap();
    write(&m.daily_path(today), "shipped reinforcement");
    let idx = enabled_index();
    idx.reindex(&m.all_chunks()).unwrap();

    // Recall the Likes chunk long ago so it decays dormant by `future`.
    idx.reinforce_at(&search_for(&idx, "rust"), T0).unwrap();
    let future = T0 + 500 * DAY_MS;

    let (block, keys) = m.ambient_block_filtered_keyed_for(&idx, today, future);
    assert!(
        !block.unwrap().contains("user prefers rust"),
        "dormant excised"
    );

    let curated: Vec<MemoryChunk> = m
        .all_chunks()
        .into_iter()
        .filter(|c| matches!(c.source, MemorySource::Curated))
        .collect();
    let likes_key = chunk_key(curated.iter().find(|c| c.text.contains("rust")).unwrap());
    let dislikes_key = chunk_key(curated.iter().find(|c| c.text.contains("verbose")).unwrap());
    // Only the live (non-dormant) curated chunk's key — no dormant key, no daily key.
    assert_eq!(keys, vec![dislikes_key], "only the live curated chunk key");
    assert!(!keys.contains(&likes_key), "dormant key excluded");
}

#[test]
fn keyed_ambient_keys_feed_reinforcement_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let m = mem(dir.path());
    write(&m.curated_path(), "## Likes\nuser prefers rust\n");
    let idx = Fts5Index::open_in_memory()
        .unwrap()
        .with_decay(DecayConfig {
            enabled: true,
            ambient_gain: 0.3,
            ..DecayConfig::default()
        });
    idx.reindex(&m.all_chunks()).unwrap();
    let today = NaiveDate::from_ymd_opt(2026, 6, 22).unwrap();

    // Recall once at T0 so the chunk is tracked (weight 1.0).
    idx.reinforce_at(&search_for(&idx, "rust"), T0).unwrap();

    // 60 days later it is still live (above dormant_threshold), so the keyed
    // ambient call surfaces its key.
    let future = T0 + 60 * DAY_MS;
    let key = m
        .ambient_block_filtered_keyed_for(&idx, today, future)
        .1
        .into_iter()
        .next()
        .expect("a live curated key");

    let before = idx
        .effective_stats(std::slice::from_ref(&key), future)
        .unwrap()[&key]
        .weight;
    // Ambient injection + reply reinforces exactly that injected key.
    idx.reinforce_ambient_at(std::slice::from_ref(&key), future)
        .unwrap();
    let after = idx
        .effective_stats(std::slice::from_ref(&key), future)
        .unwrap()[&key]
        .weight;
    assert!(
        after > before,
        "ambient reinforcement of the injected key lifted its weight ({before} -> {after})"
    );
}
