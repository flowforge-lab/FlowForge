//! `flowforge config` — list / set / clear provider credentials (#724).
//!
//! The CLI writes the same `<config_dir>/flowforge/provider-registry.json` the
//! desktop's settings panel writes, and uses the same keychain `account` scheme
//! (see [`crate::secrets`]). A `set` auto-creates a default connection of the
//! requested kind when none exists; multi-connection ambiguity and local kinds
//! error with a clear message (provider CRUD is out of scope for v1).
//!
//! ```
//! flowforge config list
//! flowforge config <provider> <secret> <value>      # store a credential
//! flowforge config <provider> <secret> --stdin      # pipe-friendly (CI)
//! flowforge config <provider> <secret> --clear      # remove a credential
//! ```
//!
//! Secret values passed as argv are visible in process listings — `--stdin` is
//! the right choice for CI/automation.

use std::io::Read;
use std::process::ExitCode;

use clap::Args;
use ff_core::{ProviderConnection, ProviderKind, ProviderRegistry, SecretKind};

use crate::registry;

/// `flowforge config <SUBCOMMAND>`.
#[derive(Debug, clap::Subcommand)]
pub enum ConfigCommand {
    /// List every connection in the registry with its secret-presence flags.
    List,
    /// Store a credential (`<value>` or `--stdin`) or remove one (`--clear`)
    /// for the connection matching `<provider>`.
    Set(SetArgs),
}

/// Arguments for `flowforge config <provider> <secret> ...`. Pulled into a
/// struct so the runner and the tests can share a single signature.
#[derive(Debug, Args)]
pub struct SetArgs {
    /// Provider kind. Secret-bearing kinds only: bedrock, openai, siliconflow.
    /// Local kinds (candle-vllm, ollama) are intentionally absent from the
    /// `ProviderKindArg` enum — the parser rejects them, and the runtime
    /// guard is a defense-in-depth backstop.
    pub provider: ProviderKindArg,
    /// Which credential on the connection: `api-key`, `secret-access-key`, or
    /// `session-token`. Some kinds accept only `api-key`; the runner rejects
    /// mismatches with a clear message.
    pub secret: SecretKindArg,
    /// The secret value. Mutually exclusive with `--stdin` and `--clear`.
    #[arg(conflicts_with_all = ["stdin", "clear"])]
    pub value: Option<String>,
    /// Read the secret value from stdin (pipe-friendly for CI). Mutually
    /// exclusive with `<value>` and `--clear`.
    #[arg(long, conflicts_with_all = ["value", "clear"], default_value_t = false)]
    pub stdin: bool,
    /// Remove the stored secret instead of writing one. Mutually exclusive
    /// with `<value>` and `--stdin`.
    #[arg(long, conflicts_with_all = ["value", "stdin"], default_value_t = false)]
    pub clear: bool,
}

/// CLI surface for provider kinds. Kebab-cased by clap; explicit `name` only
/// where the kebab-case spelling would be ugly (`OpenAi` → `openai`, not
/// `open-ai`). Local kinds are intentionally omitted — a positional provider
/// that's local errors with a clear message.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum ProviderKindArg {
    Bedrock,
    /// `openai` (not `open-ai`): the wire tag and keychain slug are both
    /// `openai`, so the CLI name should be too.
    #[clap(name = "openai")]
    OpenAi,
    /// `siliconflow`: the kebab-case form matches the slug, the issue's
    /// examples, and the registry's `vendor` descriptor.
    Siliconflow,
}

impl From<ProviderKindArg> for ProviderKind {
    fn from(arg: ProviderKindArg) -> Self {
        match arg {
            ProviderKindArg::Bedrock => ProviderKind::Bedrock,
            ProviderKindArg::OpenAi => ProviderKind::OpenAi,
            ProviderKindArg::Siliconflow => ProviderKind::SiliconFlow,
        }
    }
}

/// CLI surface for the secret kinds. Kebab-case for the shell; maps 1:1 to
/// [`SecretKind`]. `api-key` and `secret-access-key` are both Bedrock-secret
/// names; `session-token` is the AWS temporary-credential half.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum SecretKindArg {
    #[clap(name = "api-key")]
    ApiKey,
    #[clap(name = "secret-access-key")]
    SecretAccessKey,
    #[clap(name = "session-token")]
    SessionToken,
}

impl From<SecretKindArg> for SecretKind {
    fn from(arg: SecretKindArg) -> Self {
        match arg {
            SecretKindArg::ApiKey => SecretKind::ApiKey,
            SecretKindArg::SecretAccessKey => SecretKind::SecretAccessKey,
            SecretKindArg::SessionToken => SecretKind::SessionToken,
        }
    }
}

/// Which secret kinds a given provider accepts (#724 v1). Bedrock takes all
/// three (it has a notion of IAM keys + temporary session tokens); the
/// OpenAI-compatible hosted kinds only take a bearer `api-key`. Local kinds
/// take none (rejected at the caller).
fn accepted_secret_kinds(kind: ProviderKind) -> &'static [SecretKind] {
    match kind {
        ProviderKind::Bedrock => &SecretKind::ALL,
        ProviderKind::OpenAi | ProviderKind::SiliconFlow => &[SecretKind::ApiKey],
        ProviderKind::CandleVllm | ProviderKind::Ollama => &[],
    }
}

fn err(msg: impl AsRef<str>) -> ExitCode {
    eprintln!("error: {}", msg.as_ref());
    ExitCode::FAILURE
}

fn ok(msg: impl AsRef<str>) -> ExitCode {
    eprintln!("{}", msg.as_ref());
    ExitCode::SUCCESS
}

/// Validate the (kind, secret) pair before any IO. Pure — unit-testable
/// without a registry, a keychain, or clap.
fn validate_target(kind: ProviderKind, secret: SecretKind) -> Result<(), String> {
    if kind.is_local() {
        return Err(format!(
            "provider '{}' is local and has no credentials to configure; nothing to do",
            kind.slug()
        ));
    }
    if !accepted_secret_kinds(kind).contains(&secret) {
        let allowed: Vec<String> = accepted_secret_kinds(kind)
            .iter()
            .map(|k| secret_slug(*k).to_string())
            .collect();
        return Err(format!(
            "provider '{provider}' does not accept '{secret}'; allowed: {allowed}",
            provider = kind.slug(),
            secret = secret_slug(secret),
            allowed = allowed.join(", ")
        ));
    }
    Ok(())
}

fn secret_slug(kind: SecretKind) -> &'static str {
    match kind {
        SecretKind::ApiKey => "api-key",
        SecretKind::SecretAccessKey => "secret-access-key",
        SecretKind::SessionToken => "session-token",
    }
}

/// Top-level entry point: dispatches `list` and `set` to their runners. Reads
/// from stdin for `--stdin` here so the testable runners can take a
/// pre-resolved value.
pub fn run(command: ConfigCommand) -> ExitCode {
    match command {
        ConfigCommand::List => run_list(),
        ConfigCommand::Set(args) => {
            // Resolve the value source: positional, stdin, or --clear.
            let value: Option<String> = if args.clear {
                None
            } else if args.stdin {
                match read_stdin_value() {
                    Ok(v) => Some(v),
                    Err(e) => return err(e),
                }
            } else {
                match args.value.as_deref() {
                    Some(v) if !v.is_empty() => Some(v.to_string()),
                    _ => {
                        return err(
                            "missing secret value: provide one as a positional argument, \
                             pass --stdin to read from stdin, or pass --clear to remove",
                        );
                    }
                }
            };
            run_set(&args, value.as_deref())
        }
    }
}

fn read_stdin_value() -> Result<String, String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| format!("read stdin: {e}"))?;
    // Strip a single trailing newline (common when piping `echo $VALUE`); leave
    // any other whitespace alone — secrets legitimately may have leading/trailing
    // spaces and we shouldn't silently munge them.
    let trimmed = buf
        .strip_suffix("\r\n")
        .or_else(|| buf.strip_suffix('\n'))
        .unwrap_or(&buf);
    Ok(trimmed.to_string())
}

fn run_list() -> ExitCode {
    let registry = registry::load_registry();
    // Header: connection, kind, display, then one column per SecretKind (✓ / –)
    // and an `active` marker. TSV keeps this machine-parseable for
    // `flowforge config list | awk ...` style scripting.
    println!("connection\tkind\tdisplay\tapi-key\tsecret-access-key\tsession-token\tactive");
    for conn in &registry.connections {
        let present = crate::secrets::present(&conn.id);
        let api = mark(present.contains(&SecretKind::ApiKey));
        let secret = mark(present.contains(&SecretKind::SecretAccessKey));
        let session = mark(present.contains(&SecretKind::SessionToken));
        let active = if conn.id == registry.active { "*" } else { "" };
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            conn.id,
            conn.kind.slug(),
            conn.display_name,
            api,
            secret,
            session,
            active
        );
    }
    ExitCode::SUCCESS
}

fn mark(present: bool) -> &'static str {
    if present {
        "✓"
    } else {
        "–"
    }
}

/// Core set/clear body — pure, testable. `value` is `Some` to set, `None` to
/// clear. The caller (top-level [`run`]) is responsible for resolving the
/// value source; this body only deals with the loaded registry + keychain.
pub(crate) fn run_set(args: &SetArgs, value: Option<&str>) -> ExitCode {
    let kind: ProviderKind = args.provider.into();
    let secret: SecretKind = args.secret.into();

    if let Err(e) = validate_target(kind, secret) {
        return err(e);
    }

    let mut registry = registry::load_registry();
    let conn_id = match resolve_or_create(&mut registry, kind) {
        Ok(id) => id,
        Err(e) => return err(e),
    };

    if let Some(value) = value {
        // ---- set ----
        if let Err(e) = crate::secrets::set(&conn_id, secret, value) {
            return err(format!("keychain write failed: {e}"));
        }
        // The keychain is the source of truth for presence (#320); flip the
        // coarse flag so the desktop's settings panel and the CLI's `list`
        // agree on the new state.
        if let Some(conn) = registry.connections.iter_mut().find(|c| c.id == conn_id) {
            conn.has_key = true;
        }
        if let Err(e) = registry::save_registry(&registry) {
            // The secret is in the keychain but the registry didn't update —
            // surface the failure so the user knows their list view will be
            // stale until the next successful write.
            return err(format!(
                "stored secret in keychain but registry save failed: {e}"
            ));
        }
        ok(format!(
            "stored {secret} for connection '{conn_id}'",
            secret = secret_slug(secret)
        ))
    } else {
        // ---- clear ----
        if let Err(e) = crate::secrets::clear(&conn_id, secret) {
            return err(format!("keychain delete failed: {e}"));
        }
        let has_key = !crate::secrets::present(&conn_id).is_empty();
        if let Some(conn) = registry.connections.iter_mut().find(|c| c.id == conn_id) {
            conn.has_key = has_key;
        }
        if let Err(e) = registry::save_registry(&registry) {
            return err(format!(
                "cleared secret from keychain but registry save failed: {e}"
            ));
        }
        ok(format!(
            "cleared {secret} for connection '{conn_id}'",
            secret = secret_slug(secret)
        ))
    }
}

/// Find the unique connection of `kind`, auto-create one if none exists, and
/// error cleanly when more than one already exists (provider CRUD is out of
/// scope; the desktop's settings panel is the right tool to disambiguate).
/// Returns the resolved connection id.
fn resolve_or_create(
    registry: &mut ProviderRegistry,
    kind: ProviderKind,
) -> Result<String, String> {
    let matches: Vec<String> = registry
        .connections
        .iter()
        .filter(|c| c.kind == kind)
        .map(|c| c.id.clone())
        .collect();
    match matches.len() {
        1 => Ok(matches[0].clone()),
        0 => {
            // Auto-create a single-kind connection. The id is the kind slug
            // (`bedrock`, `openai`, `siliconflow`); `migrate()` runs at load
            // time so the seeded `schema_version` is current.
            let id = kind.slug().to_string();
            let conn = new_default_connection(kind, &id);
            registry.upsert(conn);
            Ok(id)
        }
        _ => Err(format!(
            "multiple connections of kind '{kind}' exist ({ids}); \
             provider CRUD is not yet supported by the CLI — remove the \
             extras via the desktop settings panel or use one explicitly",
            kind = kind.slug(),
            ids = matches.join(", ")
        )),
    }
}

/// A bare default [`ProviderConnection`] for the auto-create path. Mirrors
/// what the desktop's `provider_registry()` does on first paint: a kind, an
/// id, a display name, a sensible out-of-the-box model, and the per-kind
/// reasoning defaults. `kind` and `id` are both required; every other field
/// gets the same default the `Default` impl on `ProviderConnection` would
/// produce, spelled out so the build is self-documenting.
fn new_default_connection(kind: ProviderKind, id: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.to_string(),
        kind,
        display_name: id.to_string(),
        vendor: None,
        base_url: None,
        model: default_model_for(kind),
        has_key: false,
        secret_missing: false,
        thinking: kind.default_thinking(),
        reasoning_effort: Default::default(),
        reasoning_visibility: Default::default(),
        warmup_enabled: true,
        num_ctx: None,
        region: None,
        auth_mode: None,
        aws_profile: None,
        access_key_id: None,
        compaction_model: None,
        compaction_budget: None,
    }
}

fn default_model_for(kind: ProviderKind) -> String {
    match kind {
        // A reasonable out-of-the-box for each hosted kind; the user edits
        // it via the desktop settings panel if needed.
        ProviderKind::Bedrock => "anthropic.claude-3-5-sonnet-20241022-v2:0".to_string(),
        ProviderKind::OpenAi => "gpt-4o-mini".to_string(),
        ProviderKind::SiliconFlow => "Qwen/Qwen2.5-7B-Instruct".to_string(),
        // Local kinds are rejected upstream; unreachable in practice.
        ProviderKind::CandleVllm | ProviderKind::Ollama => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    use crate::test_support::{lock_mem_store, TestEnv};

    /// Build a `SetArgs` from raw argv via the real clap parser, so the parse
    /// table is exercised in lockstep with `main`'s command tree. The
    /// `Wrapper` here mirrors the `Config` arm of the real binary: pass
    /// `["flowforge", "set", "bedrock", "api-key", …]`.
    fn parse_set(argv: &[&str]) -> SetArgs {
        #[derive(Parser, Debug)]
        #[command(name = "flowforge")]
        struct Wrapper {
            #[command(subcommand)]
            cmd: ConfigCommand,
        }
        let parsed = Wrapper::try_parse_from(argv).expect("parses");
        match parsed.cmd {
            ConfigCommand::Set(args) => args,
            ConfigCommand::List => panic!("expected Set, got List"),
        }
    }

    #[test]
    fn validate_local_provider_is_rejected() {
        // Defense-in-depth: the parser already rejects local kinds because
        // they aren't a `ProviderKindArg` variant, but the guard must hold
        // for any code path that constructs a `ProviderKind` directly.
        for kind in [ProviderKind::CandleVllm, ProviderKind::Ollama] {
            assert!(validate_target(kind, SecretKind::ApiKey).is_err());
        }
    }

    #[test]
    fn validate_secret_matrix_matches_spec() {
        // Bedrock takes all three; OpenAI/SiliconFlow take api-key only.
        assert!(validate_target(ProviderKind::Bedrock, SecretKind::ApiKey).is_ok());
        assert!(validate_target(ProviderKind::Bedrock, SecretKind::SecretAccessKey).is_ok());
        assert!(validate_target(ProviderKind::Bedrock, SecretKind::SessionToken).is_ok());
        assert!(validate_target(ProviderKind::OpenAi, SecretKind::ApiKey).is_ok());
        assert!(validate_target(ProviderKind::OpenAi, SecretKind::SecretAccessKey).is_err());
        assert!(validate_target(ProviderKind::OpenAi, SecretKind::SessionToken).is_err());
        assert!(validate_target(ProviderKind::SiliconFlow, SecretKind::ApiKey).is_ok());
        assert!(validate_target(ProviderKind::SiliconFlow, SecretKind::SecretAccessKey).is_err());
    }

    #[test]
    fn accepted_secret_kinds_table_is_consistent() {
        // The issue (#724) lists `bedrock, openai, siliconflow` as the
        // secret-bearing kinds and `api-key, secret-access-key, session-token`
        // as the secret kinds. Verify the matrix the runner actually enforces.
        assert_eq!(
            accepted_secret_kinds(ProviderKind::Bedrock),
            &SecretKind::ALL
        );
        assert_eq!(
            accepted_secret_kinds(ProviderKind::OpenAi),
            &[SecretKind::ApiKey]
        );
        assert_eq!(
            accepted_secret_kinds(ProviderKind::SiliconFlow),
            &[SecretKind::ApiKey]
        );
        assert!(accepted_secret_kinds(ProviderKind::CandleVllm).is_empty());
        assert!(accepted_secret_kinds(ProviderKind::Ollama).is_empty());
    }

    #[test]
    fn parse_set_with_positional_value() {
        let a = parse_set(&["flowforge", "set", "bedrock", "api-key", "sk-123"]);
        assert!(matches!(a.provider, ProviderKindArg::Bedrock));
        assert!(matches!(a.secret, SecretKindArg::ApiKey));
        assert_eq!(a.value.as_deref(), Some("sk-123"));
        assert!(!a.stdin);
        assert!(!a.clear);
    }

    #[test]
    fn parse_set_with_clear_flag() {
        let a = parse_set(&["flowforge", "set", "openai", "api-key", "--clear"]);
        assert!(matches!(a.provider, ProviderKindArg::OpenAi));
        assert!(a.clear);
        assert!(a.value.is_none());
        assert!(!a.stdin);
    }

    #[test]
    fn parse_set_with_stdin_flag() {
        let a = parse_set(&["flowforge", "set", "siliconflow", "api-key", "--stdin"]);
        assert!(matches!(a.provider, ProviderKindArg::Siliconflow));
        assert!(a.stdin);
        assert!(a.value.is_none());
        assert!(!a.clear);
    }

    #[test]
    fn parse_set_rejects_value_and_clear_together() {
        #[derive(Parser, Debug)]
        #[command(name = "flowforge")]
        struct Wrapper {
            #[command(subcommand)]
            cmd: ConfigCommand,
        }
        let err = Wrapper::try_parse_from([
            "flowforge",
            "set",
            "bedrock",
            "api-key",
            "sk-123",
            "--clear",
        ])
        .expect_err("value + --clear must conflict");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn parse_set_rejects_stdin_and_clear_together() {
        #[derive(Parser, Debug)]
        #[command(name = "flowforge")]
        struct Wrapper {
            #[command(subcommand)]
            cmd: ConfigCommand,
        }
        let err = Wrapper::try_parse_from([
            "flowforge",
            "set",
            "bedrock",
            "api-key",
            "--stdin",
            "--clear",
        ])
        .expect_err("--stdin + --clear must conflict");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn parse_list_does_not_require_provider() {
        #[derive(Parser, Debug)]
        #[command(name = "flowforge")]
        struct Wrapper {
            #[command(subcommand)]
            cmd: ConfigCommand,
        }
        Wrapper::try_parse_from(["flowforge", "list"]).expect("list parses without a provider");
    }

    #[test]
    fn parse_set_rejects_unknown_secret_kind() {
        #[derive(Parser, Debug)]
        #[command(name = "flowforge")]
        struct Wrapper {
            #[command(subcommand)]
            cmd: ConfigCommand,
        }
        let err =
            Wrapper::try_parse_from(["flowforge", "set", "bedrock", "no-such-secret", "sk-123"])
                .expect_err("unknown secret kind must be rejected");
        // An unknown `SecretKindArg` is a subcommand-level argument error, not
        // a value error: clap rejects it before the value-level validation
        // runs. Either is acceptable — the contract is just that it's not
        // a silent no-op.
        assert!(
            matches!(
                err.kind(),
                clap::error::ErrorKind::InvalidValue | clap::error::ErrorKind::UnknownArgument
            ),
            "expected InvalidValue or UnknownArgument, got {:?}",
            err.kind()
        );
    }

    // ---- end-to-end runner tests, all sharing one in-process keychain ----
    //
    // The runner reads/writes the OS keychain via `secrets::*`, which uses a
    // process-global `OnceLock<MemStore>` under `cfg(test)`. To keep parallel
    // test threads from stepping on each other's accounts, the integration
    // tests take a process-global `MEM_STORE_LOCK` and use per-test ids
    // (`TestEnv::new()` returns a unique tempdir) so they don't collide on
    // the registry path either.

    #[test]
    fn set_then_list_reflects_has_key() {
        let _lock = lock_mem_store();
        let env = TestEnv::new();
        env.write_registry(&ProviderRegistry::default());

        let args = parse_set(&["flowforge", "set", "bedrock", "api-key", "sk-test"]);
        let code = run_set(&args, Some("sk-test"));
        assert_eq!(code, ExitCode::SUCCESS);

        let reg = registry::load_registry_at(Some(env.registry_path()), Some(env.legacy_path()));
        let conn = reg
            .connections
            .iter()
            .find(|c| c.kind == ProviderKind::Bedrock)
            .expect("auto-created bedrock connection");
        assert!(conn.has_key, "has_key should be flipped to true");
        assert_eq!(
            crate::secrets::get(&conn.id, SecretKind::ApiKey).as_deref(),
            Some("sk-test")
        );
    }

    #[test]
    fn clear_resets_has_key_when_no_other_secrets_remain() {
        let _lock = lock_mem_store();
        let env = TestEnv::new();
        env.write_registry(&ProviderRegistry::default());

        let set_args = parse_set(&["flowforge", "set", "bedrock", "api-key", "sk-test"]);
        run_set(&set_args, Some("sk-test"));
        let clear_args = parse_set(&["flowforge", "set", "bedrock", "api-key", "--clear"]);
        let code = run_set(&clear_args, None);
        assert_eq!(code, ExitCode::SUCCESS);

        let reg = registry::load_registry_at(Some(env.registry_path()), Some(env.legacy_path()));
        let conn = reg
            .connections
            .iter()
            .find(|c| c.kind == ProviderKind::Bedrock)
            .unwrap();
        assert!(!conn.has_key, "has_key should be false after clear");
        assert!(crate::secrets::get(&conn.id, SecretKind::ApiKey).is_none());
    }

    #[test]
    fn set_rejects_invalid_secret_for_provider() {
        let _lock = lock_mem_store();
        let env = TestEnv::new();
        env.write_registry(&ProviderRegistry::default());

        // OpenAI accepts only api-key; session-token is Bedrock-only.
        let args = parse_set(&["flowforge", "set", "openai", "session-token", "tok"]);
        let code = run_set(&args, Some("tok"));
        assert_eq!(code, ExitCode::FAILURE);

        let reg = registry::load_registry_at(Some(env.registry_path()), Some(env.legacy_path()));
        // We errored before reaching resolve_or_create, so no openai
        // connection was auto-created.
        assert!(reg
            .connections
            .iter()
            .all(|c| c.kind != ProviderKind::OpenAi));
    }

    #[test]
    fn auto_creates_singleton_connection_for_unseen_kind() {
        let _lock = lock_mem_store();
        let env = TestEnv::new();
        env.write_registry(&ProviderRegistry::default());

        // Default registry has no siliconflow connection. Running `set` for
        // siliconflow should auto-create one rather than fail with "no such
        // connection".
        let args = parse_set(&["flowforge", "set", "siliconflow", "api-key", "sk-sf"]);
        let code = run_set(&args, Some("sk-sf"));
        assert_eq!(code, ExitCode::SUCCESS);

        let reg = registry::load_registry_at(Some(env.registry_path()), Some(env.legacy_path()));
        let conn = reg
            .connections
            .iter()
            .find(|c| c.kind == ProviderKind::SiliconFlow)
            .expect("auto-created siliconflow connection");
        assert_eq!(conn.id, "siliconflow");
        assert!(conn.has_key);
    }

    #[test]
    fn set_errors_when_multiple_connections_of_same_kind_exist() {
        let _lock = lock_mem_store();
        let env = TestEnv::new();
        let mut reg = ProviderRegistry::default();
        // Force two siliconflow connections.
        reg.connections.push(blank_provider_connection(
            "siliconflow",
            ProviderKind::SiliconFlow,
            "Qwen/Qwen2.5-7B-Instruct",
        ));
        reg.connections.push(blank_provider_connection(
            "siliconflow-2",
            ProviderKind::SiliconFlow,
            "Qwen/Qwen2.5-7B-Instruct",
        ));
        env.write_registry(&reg);

        let args = parse_set(&["flowforge", "set", "siliconflow", "api-key", "sk"]);
        let code = run_set(&args, Some("sk"));
        assert_eq!(code, ExitCode::FAILURE);
    }

    fn blank_provider_connection(id: &str, kind: ProviderKind, model: &str) -> ProviderConnection {
        ProviderConnection {
            id: id.to_string(),
            kind,
            display_name: id.to_string(),
            vendor: None,
            base_url: None,
            model: model.to_string(),
            has_key: false,
            secret_missing: false,
            thinking: kind.default_thinking(),
            reasoning_effort: Default::default(),
            reasoning_visibility: Default::default(),
            warmup_enabled: true,
            num_ctx: None,
            region: None,
            auth_mode: None,
            aws_profile: None,
            access_key_id: None,
            compaction_model: None,
            compaction_budget: None,
        }
    }
}
