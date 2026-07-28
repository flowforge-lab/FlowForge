//! Retrieval-quality regression suite for [`ToolSearchTool`] (RFC 0024 Phase 2A).
//!
//! Phase 1 shipped just-in-time tool loading with no way to tell whether a change
//! to scoring helped or hurt, so retrieval quality was argued rather than
//! measured. This suite is that missing instrument.
//!
//! # Why a synthetic corpus
//!
//! The corpus is modelled on a real deferred-tool set captured from a live
//! session, then rewritten to be vendor-neutral. Rewriting deliberately preserves
//! the five properties that make retrieval *hard*, because a corpus that loses
//! them measures an easier problem than the real one:
//!
//! 1. **A high-frequency term shared by many tools** — several tools' names and
//!    descriptions contain "search". Without IDF this term alone decides the
//!    winner for every search-shaped query.
//! 2. **Wide variance in description length** — a verbose description is
//!    mechanically more likely to contain a common query word, so without length
//!    normalisation long tools surface for queries they do not serve.
//! 3. **Tools whose *name* contains a high-frequency term** — this is what let a
//!    name-match bonus overwhelm a genuinely better description match.
//! 4. **Polymorphic tools whose real capability lives only in an `action` enum**,
//!    not in prose — the single biggest recall gap recorded in Phase 1.
//! 5. **Genuine vocabulary gaps** — queries whose wording appears in no tool's
//!    text at all, which no lexical scorer can close and which motivate both the
//!    synonym table and the substring fallback.
//!
//! # What is asserted
//!
//! `top-5`, not `top-1`. [`MAX_HITS`] is 5 and the model receives a list of five
//! candidates to choose from, so retrieval's job is to put the right tool *in the
//! list*; ranking it first is a nicety. Gating on top-1 would optimise the wrong
//! objective — and top-1 is also the metric most polluted by the fact that the
//! "correct" tool for a query is a human judgement, several of which are
//! defensibly ambiguous.

use super::*;

/// `(name, description, action enum values)`.
type ToolFixture = (&'static str, &'static str, &'static [&'static str]);

const CORPUS: &[ToolFixture] = &[
    // --- deployment / infrastructure: polymorphic, long description ---
    (
        "deploy_read",
        "A tool for reading data from the deployment system. Use for reading environment, stage, \
         deployment, capacity, and configuration data. Supports describing an environment or a \
         single stage of it, listing deployments for an environment stage, inspecting capacity, \
         and reading the audit log of configuration changes.",
        &[
            "describe-environment",
            "describe-environment-stage",
            "describe-deployment",
            "list-deployments-for-environment-stage",
            "describe-capacity",
            "list-participating-hosts-for-deployment",
            "list-audit-log-for-environment-and-stages",
        ],
    ),
    (
        "fleet_run",
        "Execute a command against a host, hostclass, or container task. Single resource type per \
         invocation. Cloud resources require an account and a role name. Clusters are addressed by \
         short region codes.",
        &[],
    ),
    // --- issue tracking: read and write split, both polymorphic ---
    (
        "tracker_read",
        "A tool for reading data from the issue tracker. Supports searching tickets, retrieving \
         ticket details, and querying resolver group information.",
        &[
            "search-tickets",
            "get-ticket",
            "get-my-resolver-groups",
            "get-resolver-group-details",
        ],
    ),
    (
        "tracker_write",
        "A tool for writing to the issue tracker. Actions: create-ticket files a new ticket, \
         update-ticket modifies ticket fields, add-comment posts a comment, update-comment edits \
         an existing comment you authored. Severity ranges from sev-1 highest to sev-5 lowest.",
        &[
            "create-ticket",
            "update-ticket",
            "add-comment",
            "update-comment",
        ],
    ),
    // --- CI/CD: two tools that are easy to confuse with each other ---
    (
        "pipeline_health",
        "Retrieves the current status and health metrics for a list of pipelines. The response \
         includes whether the pipeline is enabled, the fitness badge, health metrics like failed \
         builds, failed deployments and failed tests, pending approvals, and pending workflow \
         steps. A pipeline with any non-zero health metric is blocked and needs operator \
         intervention.",
        &[],
    ),
    (
        "pipeline_details",
        "Returns a detailed summary of one pipeline's current state, including its name, \
         identifier, description, enabled status, stage count, target count, and the latest events \
         for each target.",
        &[],
    ),
    (
        "local_build",
        "Diagnoses and analyzes build executions in local workspaces. Runs the build in the \
         specified directory and reports success or failure. On failure, performs analysis \
         including root cause identification, relevant file pointers, and step-by-step \
         recommendations. Can also run unit tests and linters.",
        &[],
    ),
    (
        "remote_build",
        "Analyzes build failures on the shared build service. Fetches build logs for a build \
         request and provides detailed analysis of any errors encountered. Use when a build failed \
         remotely rather than on your workstation.",
        &[],
    ),
    (
        "workspace_create",
        "Creates a new workspace for the specified packages. A workspace is a container for one or \
         more packages you want to work on, providing isolation between different development \
         activities.",
        &[],
    ),
    // --- code search / review: the "search" term collision lives here ---
    (
        "code_search",
        "Search source code across repositories. Semantic search returns the full source of \
         matching code elements. Code search returns snippets matching exact strings, symbols, or \
         boolean queries. Repository search returns matching repositories by name.",
        &["code", "repositories", "semantic"],
    ),
    (
        "review_create",
        "Creates a new code review revision from a workspace, or updates an existing code review. \
         A code review tracks proposed changes to software packages before they are merged.",
        &[],
    ),
    (
        "review_comment",
        "Posts a comment or reply on an existing code review. Reply to a reviewer's comment, add a \
         top-level comment, or add an inline comment anchored to a file and line.",
        &[],
    ),
    (
        "review_checkout",
        "Checks out an existing code review by identifier for local editing. Use only when you \
         need to modify or build the review's code locally.",
        &[],
    ),
    // --- oncall / risk ---
    (
        "oncall_read",
        "A tool for reading data from the on-call system. Supports searching teams, listing the \
         teams a user belongs to, viewing shifts, and generating on-call reports.",
        &[
            "search-teams",
            "list-user-teams",
            "get-user-shifts",
            "get-team-shifts",
        ],
    ),
    (
        "risk_read",
        "Reads security risks from the assurance service. Get risks for a user, a summary of a \
         user's risks, risks for a dependency version set, risks for a pipeline, or risks for a \
         deployment resource.",
        &["get-user-risks", "get-pipeline-risks", "get-apollo-risks"],
    ),
    // --- test results: polymorphic via a non-`action` discriminator ---
    (
        "test_run_read",
        "Read metadata, logs, artifacts and history for a remote test run. Service logs for error \
         detection and troubleshooting, the main test output log, test result files, and test \
         history across runs.",
        &["service-logs", "logs", "artifacts", "history", "summary"],
    ),
    // --- documents / tasks ---
    (
        "doc_editor",
        "Retrieves and edits collaborative documents. Create a new document from a file, read a \
         document with its structure, insert content after a heading, append to a document, or \
         replace a section.",
        &[],
    ),
    (
        "task_create",
        "Create a new task. Allows setting a name, description, assignee, room identifier, and an \
         optional need-by date.",
        &[],
    ),
    (
        "task_list",
        "List tasks. Allows querying tasks using natural language descriptions of filters. Use \
         when asked about listing, filtering, or finding tasks.",
        &[],
    ),
    (
        "wiki_search",
        "Search the internal knowledge engine. Available domains include documentation, \
         company-wide announcements, engineering question-and-answer sites, internal policy, and \
         the internal wiki.",
        &[],
    ),
    (
        "intranet_read",
        "Reads content from internal websites: code review pages, package trees and file blobs, \
         collaborative documents, issue pages, the employee directory, wiki pages, and pipeline \
         pages.",
        &[],
    ),
    (
        "acronym_lookup",
        "Search the internal acronym database. Returns definitions with exact match search, full \
         definitions with source links, and associated tags for context.",
        &[],
    ),
    // --- personal notes: short descriptions, and the vocabulary gaps ---
    ("vault_search", "Search vault for text.", &[]),
    (
        "vault_search_context",
        "Search with matching line context.",
        &[],
    ),
    ("daily_append", "Append content to daily note.", &[]),
    ("daily_read", "Read daily note contents.", &[]),
    ("file_create", "Create a new file.", &[]),
    ("tag_list", "List tags in the vault.", &[]),
    ("task_vault_list", "List tasks in the vault.", &[]),
    ("backlink_list", "List backlinks to a file.", &[]),
    ("word_count", "Count words and characters.", &[]),
    // --- code navigation: the "understand" vocabulary gap ---
    (
        "symbol_explore",
        "Returns the verbatim source of the relevant symbols grouped by file in one capped call, \
         plus the call path among them. The query can be a natural-language question or a bag of \
         symbol and file names.",
        &[],
    ),
];

/// Queries phrased the way a model phrases them, each paired with the tool a
/// human would call correct.
///
/// A handful are deliberately ambiguous — "run unit tests for a package" could
/// defend either build tool, and "pending approvals" either pipeline tool. They
/// are kept because real queries are ambiguous; they are also the reason the gate
/// is top-5 rather than top-1.
const QUERIES: &[(&str, &str)] = &[
    ("list recent deployments", "deploy_read"),
    ("deployment history for an environment", "deploy_read"),
    ("why did my deployment fail", "deploy_read"),
    ("check environment config", "deploy_read"),
    ("how much capacity does this fleet have", "deploy_read"),
    ("which hosts took part in a deploy", "deploy_read"),
    ("search tickets", "tracker_read"),
    ("find open tickets for my team", "tracker_read"),
    ("read a ticket", "tracker_read"),
    ("file a new ticket", "tracker_write"),
    ("create a sev 3", "tracker_write"),
    ("comment on a ticket", "tracker_write"),
    ("who is the resolver group", "tracker_read"),
    ("is my pipeline blocked", "pipeline_health"),
    ("pipeline health", "pipeline_health"),
    ("failed builds in the pipeline", "pipeline_health"),
    ("show me pipeline details", "pipeline_details"),
    ("pending approvals", "pipeline_health"),
    ("search source code", "code_search"),
    ("find a function in the codebase", "code_search"),
    ("look for a package by name", "code_search"),
    ("create a code review", "review_create"),
    ("publish a code review", "review_create"),
    ("reply to a review comment", "review_comment"),
    ("check out a code review locally", "review_checkout"),
    ("why did my build fail", "local_build"),
    ("run the build", "local_build"),
    ("build failure on the build service", "remote_build"),
    ("analyze a build request", "remote_build"),
    ("create a workspace", "workspace_create"),
    ("run unit tests for a package", "local_build"),
    ("who is on call", "oncall_read"),
    ("my oncall shifts", "oncall_read"),
    ("oncall schedule for a team", "oncall_read"),
    ("security risks assigned to me", "risk_read"),
    ("risks for a pipeline", "risk_read"),
    ("run a command on a host", "fleet_run"),
    ("check a cloud instance", "fleet_run"),
    ("read test run logs", "test_run_read"),
    ("why did the integration test fail", "test_run_read"),
    ("test artifacts", "test_run_read"),
    ("edit a collaborative doc", "doc_editor"),
    ("create a task", "task_create"),
    ("list my tasks", "task_list"),
    ("search the internal wiki", "wiki_search"),
    ("read a wiki page", "intranet_read"),
    ("what does this acronym mean", "acronym_lookup"),
    ("search my notes", "vault_search"),
    ("append to my journal", "daily_append"),
    ("what did I write today", "daily_read"),
    ("list my tags", "tag_list"),
    ("understand how a function works", "symbol_explore"),
];

/// Queries written *after* the scorer was tuned, and never used to tune it.
///
/// [`QUERIES`] was written before any result was seen, but it was then looked at
/// repeatedly while iterating on the scorer, the stop-word list and especially the
/// synonym table — several synonym entries exist precisely because a query in that
/// set missed. It is therefore an open-book exam, and its scores are an upper
/// bound, not an estimate of field behaviour.
///
/// This set is the closed-book control. Same corpus, same wording style, but no
/// mechanism was added or adjusted in response to it. The gap between the two is
/// the honest measure of how much of the headline number is fitting:
///
/// | set | top-1 | top-5 |
/// |---|---|---|
/// | [`QUERIES`] (open book) | 90.4% | 100% |
/// | [`HELD_OUT`] (closed book) | 25.0% | 46.4% |
///
/// Both are asserted, separately, so the gap stays visible instead of being
/// averaged away. A future change that lifts the held-out floor is real progress;
/// one that only lifts the tuned floor is more fitting.
///
/// Three of the held-out misses (`escalate an incident`, `approve a promotion`,
/// `ssh into a box`) return *nothing at all*, and are unreachable by construction:
/// they share no token with their target's text, so no lexical scorer — BM25,
/// substring, or otherwise — can connect them. They are kept in the set precisely
/// because they quantify the ceiling of the lexical approach, which is the
/// evidence needed to judge whether a semantic layer earns its cost.
const HELD_OUT: &[(&str, &str)] = &[
    ("roll back a bad release", "deploy_read"),
    ("what version is deployed", "deploy_read"),
    ("show me the stage config", "deploy_read"),
    ("escalate an incident", "tracker_write"),
    ("assign this to someone else", "tracker_write"),
    ("close out a bug report", "tracker_write"),
    ("did the release go out", "pipeline_health"),
    ("approve a promotion", "pipeline_health"),
    ("grep the repo", "code_search"),
    ("where is this class defined", "code_search"),
    ("who reviewed my change", "review_comment"),
    ("merge my change", "review_create"),
    ("compile errors", "local_build"),
    ("lint my package", "local_build"),
    ("flaky test", "test_run_read"),
    ("stack trace from a failed run", "test_run_read"),
    ("page the on call engineer", "oncall_read"),
    ("swap shifts with a teammate", "oncall_read"),
    ("vulnerabilities in my service", "risk_read"),
    ("ssh into a box", "fleet_run"),
    ("restart a container", "fleet_run"),
    ("take meeting notes", "doc_editor"),
    ("what am I working on", "task_list"),
    ("look up a term", "acronym_lookup"),
    ("how many words did I write", "word_count"),
    ("what links to this page", "backlink_list"),
    ("open my todo list", "task_vault_list"),
    ("trace a call path", "symbol_explore"),
];

/// Minimum share of [`QUERIES`] whose correct tool must appear in the returned list.
///
/// This is the metric that matters for the model's *outcome* — see the module
/// docs — but it is a weak guard on the *scorer*: with BM25F disabled entirely,
/// top-5 stays at 100% because the substring fallback still fills five slots.
/// Verified by mutation.
const TOP5_FLOOR: f64 = 0.96;

/// Minimum share of [`QUERIES`] whose correct tool must rank *first*.
///
/// Kept as a second, lower gate because top-5 alone cannot detect the ranking
/// quality BM25F exists to provide: removing BM25F costs 12 points of top-1
/// (88.5% → 76.9%) while leaving top-5 untouched. Without this assertion the
/// suite would pass on a scorer that had been gutted.
///
/// Set below the measured 88.5% because several queries are defensibly ambiguous
/// (either build tool for "run unit tests", either pipeline tool for "pending
/// approvals"), so small movement is annotation noise. A 12-point drop is not.
const TOP1_FLOOR: f64 = 0.84;

/// Floors for [`HELD_OUT`], set just under the measured 46.4% / 25.0%.
///
/// Deliberately recorded as low numbers rather than quietly omitted. They are
/// what lexical retrieval actually achieves on wording it was not fitted to, and
/// the honest baseline for judging whether a semantic layer is worth its cost.
const HELD_OUT_TOP5_FLOOR: f64 = 0.42;
const HELD_OUT_TOP1_FLOOR: f64 = 0.21;

struct Fixture(&'static str, &'static str, Value);

#[async_trait]
impl Tool for Fixture {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        self.1
    }
    fn parameters(&self) -> Value {
        self.2.clone()
    }
    fn defer(&self) -> bool {
        true
    }
    async fn run(&self, _args: Value, _root: &Path) -> ToolOutcome {
        ToolOutcome::ok("ran")
    }
}

fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

fn corpus_index() -> ToolSearchTool {
    let mut registry = ToolRegistry::default();
    for (name, description, actions) in CORPUS {
        let schema = if actions.is_empty() {
            json!({ "type": "object", "properties": {} })
        } else {
            json!({
                "type": "object",
                "properties": { "action": { "type": "string", "enum": actions } }
            })
        };
        registry.register(Box::new(Fixture(name, description, schema)));
    }
    ToolSearchTool::new(
        Arc::new(ToolSearchState::new()),
        ToolSearchIndex::from_registry(&registry),
    )
}

/// Tool names returned for `query`, best first.
async fn ranked(tool: &ToolSearchTool, query: &str) -> Vec<String> {
    let text = tool
        .run_with_session(json!({ "query": query }), Path::new("."), "probe")
        .await
        .content;
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let line = line.strip_prefix("- ").unwrap_or(line);
            let candidate = line.split_whitespace().next()?;
            let candidate = candidate
                .trim_end_matches([':', '`'])
                .trim_start_matches('`');
            CORPUS
                .iter()
                .any(|(name, _, _)| *name == candidate)
                .then(|| candidate.to_string())
        })
        .collect()
}

/// `(top-1 rate, top-5 rate, misses)` for a query set.
async fn measure(set: &[(&str, &str)]) -> (f64, f64, Vec<String>) {
    let tool = corpus_index();
    let mut first = 0usize;
    let mut within = 0usize;
    let mut missed = Vec::new();

    for (query, expected) in set {
        let ranked = ranked(&tool, query).await;
        if ranked.first().is_some_and(|n| n == expected) {
            first += 1;
        }
        if ranked.iter().take(MAX_HITS).any(|n| n == expected) {
            within += 1;
        } else {
            missed.push(format!(
                "{query:?} wanted {expected} got {:?}",
                ranked.first().map(String::as_str).unwrap_or("<nothing>")
            ));
        }
    }

    let n = set.len() as f64;
    (first as f64 / n, within as f64 / n, missed)
}

#[tokio::test]
async fn tuned_set_top5_recall_stays_above_the_floor() {
    let (_, top5, missed) = measure(QUERIES).await;
    assert!(
        top5 >= TOP5_FLOOR,
        "tuned-set top-5 {:.1}% fell below the {:.0}% floor; misses: {missed:#?}",
        top5 * 100.0,
        TOP5_FLOOR * 100.0
    );
}

/// Guards the ranking quality that top-5 cannot see.
#[tokio::test]
async fn tuned_set_top1_precision_stays_above_the_floor() {
    let (top1, _, missed) = measure(QUERIES).await;
    assert!(
        top1 >= TOP1_FLOOR,
        "tuned-set top-1 {:.1}% fell below the {:.0}% floor; misses: {missed:#?}",
        top1 * 100.0,
        TOP1_FLOOR * 100.0
    );
}

/// The honest number: wording the scorer was never fitted to.
///
/// Asserted separately from the tuned set so the gap between them stays on the
/// record. A change that lifts this floor is real; one that lifts only the tuned
/// floor is more overfitting.
#[tokio::test]
async fn held_out_set_stays_above_its_floor() {
    let (top1, top5, missed) = measure(HELD_OUT).await;
    assert!(
        top5 >= HELD_OUT_TOP5_FLOOR && top1 >= HELD_OUT_TOP1_FLOOR,
        "held-out top-1 {:.1}% / top-5 {:.1}% fell below the {:.0}% / {:.0}% floors; \
         misses: {missed:#?}",
        top1 * 100.0,
        top5 * 100.0,
        HELD_OUT_TOP1_FLOOR * 100.0,
        HELD_OUT_TOP5_FLOOR * 100.0
    );
}

/// Pins the *gap* between the two sets.
///
/// The tuned set scoring far above the held-out set is expected and documented;
/// what must not happen silently is the gap widening, which is what fitting a new
/// mechanism to the tuned queries looks like from the outside. Generous ceiling —
/// this is a smoke alarm, not a thermostat.
#[tokio::test]
async fn overfitting_gap_does_not_widen() {
    let (_, tuned, _) = measure(QUERIES).await;
    let (_, held, _) = measure(HELD_OUT).await;
    let gap = tuned - held;
    assert!(
        gap <= 0.60,
        "tuned top-5 {:.1}% vs held-out {:.1}% is a {:.1}-point gap; a widening gap means \
         the mechanism is being fitted to the tuned queries rather than generalising",
        tuned * 100.0,
        held * 100.0,
        gap * 100.0
    );
}

/// The property the substring fallback exists to guarantee. An empty result is
/// the one outcome worse than an imprecise one: a wrong hit is visible to the
/// model and prompts another search, whereas nothing at all invites it to give up
/// on a capability that is in fact available.
#[tokio::test]
async fn every_query_returns_at_least_one_candidate() {
    let tool = corpus_index();
    for (query, _) in QUERIES {
        assert!(
            !ranked(&tool, query).await.is_empty(),
            "query {query:?} returned no candidate at all"
        );
    }
}

/// Pins the substring fallback, which the 52-query suite cannot reach.
///
/// Measured: *no* query in the suite falls through to it, because the synonym
/// table already bridges every vocabulary gap those queries contain — so removing
/// the fallback entirely leaves the whole suite green. Its real value is for
/// queries the suite does not contain: a partial word, or a term related to a
/// capability by substring rather than by whole token.
///
/// Without this test the fallback would be unguarded dead code, and the next
/// person to read `search()` would have no way to tell whether it still mattered.
#[tokio::test]
async fn a_partial_word_still_finds_a_tool_through_the_fallback() {
    let tool = corpus_index();

    // "pipelin" is not a whole token anywhere, so BM25F scores zero across the
    // corpus and only the substring path can answer.
    let terms = expand_query("pipelin");
    assert!(
        tool.index
            .tools
            .iter()
            .all(|t| tool.index.bm25f(t, &terms) == 0.0),
        "precondition: BM25F must find nothing, otherwise this is not testing the fallback"
    );

    let hits = ranked(&tool, "pipelin").await;
    assert!(
        hits.iter().any(|n| n.starts_with("pipeline_")),
        "the fallback must still surface a pipeline tool for a partial word; got {hits:?}"
    );
}

/// Pins IDF directly.
///
/// The corpus-level metrics cannot see this: replacing the IDF factor with a
/// constant leaves both top-1 and top-5 above their floors, because only a
/// handful of the 52 queries hinge on it. Yet discounting ubiquitous terms is the
/// main reason for the change — it is what stops the one tool whose *name*
/// contains a common word from winning every query containing it.
///
/// Asserted as a property rather than a corpus statistic: a term carried by every
/// document must contribute strictly less than a term carried by one.
#[tokio::test]
async fn ubiquitous_terms_score_below_distinctive_ones() {
    // Every tool is about "data"; only one mentions "kangaroo".
    let mut registry = ToolRegistry::default();
    for name in ["alpha", "beta", "gamma", "delta"] {
        registry.register(Box::new(Fixture(
            name,
            "handles data records",
            empty_schema(),
        )));
    }
    registry.register(Box::new(Fixture(
        "zeta",
        "handles data records about a kangaroo",
        empty_schema(),
    )));
    let index = ToolSearchIndex::from_registry(&registry);
    let zeta = index
        .tools
        .iter()
        .find(|t| t.name == "zeta")
        .expect("indexed");

    let ubiquitous = index.bm25f(zeta, &["data".to_string()]);
    let distinctive = index.bm25f(zeta, &["kangaroo".to_string()]);

    assert!(
        distinctive > ubiquitous * 2.0,
        "a term unique to one tool ({distinctive}) must outweigh one shared by all \
         ({ubiquitous}); IDF is not being applied"
    );
}

/// Pins length normalisation, for the same reason: a verbose description is
/// mechanically more likely to contain any given query word, and without this
/// factor it out-scores a short, precise description on the strength of sheer
/// volume alone.
#[tokio::test]
async fn verbose_tools_do_not_outrank_precise_ones_on_a_shared_term() {
    let mut registry = ToolRegistry::default();
    registry.register(Box::new(Fixture(
        "precise",
        "reads a ticket",
        empty_schema(),
    )));
    registry.register(Box::new(Fixture(
        "verbose",
        "reads a ticket and also manages environments, capacity, deployments, pipelines, \
         schedules, documents, reviews, packages, workspaces, artifacts and many other \
         unrelated concerns across a broad surface area",
        empty_schema(),
    )));
    let index = ToolSearchIndex::from_registry(&registry);
    let find = |name: &str| {
        index
            .tools
            .iter()
            .find(|t| t.name == name)
            .expect("indexed")
    };
    let terms = vec!["ticket".to_string()];

    assert!(
        index.bm25f(find("precise"), &terms) > index.bm25f(find("verbose"), &terms),
        "the short, on-topic tool must win on a term both contain; \
         length normalisation is not being applied"
    );
}

/// Well-formedness of the synonym table, checked for every entry.
///
/// The table is hand-maintained and will grow as real misses appear, so the
/// failure modes worth pinning are the ones a future edit would introduce
/// silently: an expansion that the tokenizer immediately discards, a key that can
/// never be produced by the tokenizer in the first place, or a self-reference that
/// adds nothing.
#[test]
fn synonym_table_entries_are_all_usable() {
    for (key, expansions) in SYNONYMS {
        assert!(
            !STOP_WORDS.contains(key),
            "{key:?} is a stop word, so the tokenizer drops it before expansion can apply"
        );
        assert_eq!(
            terms_of(key),
            vec![key.to_string()],
            "{key:?} does not survive tokenization as a single term, so it can never match"
        );
        assert!(
            !expansions.is_empty(),
            "{key:?} expands to nothing; remove the entry instead"
        );
        for expansion in *expansions {
            assert!(
                !STOP_WORDS.contains(expansion),
                "{key:?} expands to stop word {expansion:?}, which is dropped before scoring"
            );
            assert_eq!(
                terms_of(expansion),
                vec![expansion.to_string()],
                "{key:?} expands to {expansion:?}, which does not survive tokenization"
            );
            assert_ne!(
                key, expansion,
                "{key:?} expands to itself, which adds no recall"
            );
        }
    }
}

/// Expansion must only ever *add* recall.
///
/// A future edit could turn the table into a substitution — replacing the user's
/// word with a synonym — which would silently lose the exact match. Pinned per
/// entry because that regression is invisible in aggregate metrics.
#[test]
fn expansion_never_drops_the_original_term() {
    for (key, _) in SYNONYMS {
        let expanded = expand_query(key);
        assert!(
            expanded.contains(&(*key).to_string()),
            "expanding {key:?} lost the original term: {expanded:?}"
        );
    }
}

/// The three vocabulary gaps that motivated the table, asserted end to end.
///
/// These queries returned the wrong tool before the table existed: their wording
/// appears in no tool's text, so no amount of scoring could reach the right one.
#[tokio::test]
async fn measured_vocabulary_gaps_now_reach_their_tool() {
    let tool = corpus_index();
    for (query, expected) in [
        ("understand how a function works", "symbol_explore"),
        ("what did I write today", "daily_read"),
        ("append to my journal", "daily_append"),
    ] {
        let hits = ranked(&tool, query).await;
        assert!(
            hits.iter().take(MAX_HITS).any(|n| n == expected),
            "query {query:?} should reach {expected:?}; got {hits:?}"
        );
    }
}

/// Searching by a tool's exact name must find that tool.
///
/// Guards a real behaviour change in this rework: the tokenizer now splits on `_`
/// (so that `daily_append` also matches a query for "append"), where the previous
/// one kept underscores and relied on whole-name comparison. A model that has seen
/// a tool name in an earlier turn and searches for it verbatim is a normal case,
/// and it must not regress as a side effect of that split.
#[tokio::test]
async fn an_exact_tool_name_finds_that_tool() {
    let tool = corpus_index();
    for name in [
        "daily_append",
        "pipeline_health",
        "code_search",
        "deploy_read",
    ] {
        let hits = ranked(&tool, name).await;
        assert_eq!(
            hits.first().map(String::as_str),
            Some(name),
            "searching the exact name {name:?} must rank it first; got {hits:?}"
        );
    }
}

/// The empty-result message must not talk the model out of searching again.
///
/// `tool_search` is the only gateway to a deferred tool, and the tool itself is
/// always resident, so a miss costs a turn rather than the capability — *provided*
/// the model tries again. The previous wording ("proceed with the tools you
/// already have") worked directly against that: it is concrete, in-turn advice to
/// give up, competing with the tool description's general instruction not to
/// assume a capability is absent. Held-out measurement makes this load-bearing —
/// several queries return nothing at all, so this message is what the model sees
/// for a capability that is in fact present.
#[tokio::test]
async fn the_empty_result_message_pushes_the_model_to_retry() {
    let tool = corpus_index();
    let text = tool
        .run_with_session(
            json!({ "query": "zzzz nonexistent capability" }),
            Path::new("."),
            "s",
        )
        .await;

    assert!(
        text.success,
        "a miss must not be reported as a tool failure"
    );
    let body = text.content.to_lowercase();

    assert!(
        !body.contains("proceed with the tools you already have"),
        "must not advise giving up: {body}"
    );
    assert!(
        body.contains("does not mean the capability is missing"),
        "must state that a miss is not an absence: {body}"
    );
    assert!(
        body.contains("search again"),
        "must ask for another attempt: {body}"
    );
    assert!(
        body.contains(&CORPUS.len().to_string()),
        "must report how many tools remain unloaded, so 'try again' has a reason: {body}"
    );
}
