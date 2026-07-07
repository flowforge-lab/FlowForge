use super::*;

#[test]
fn default_is_tavily_keyless() {
    let cfg = SearchConfig::default();
    assert_eq!(cfg.backend, SearchBackend::Tavily);
    assert_eq!(cfg.base_url, None);
    assert!(!cfg.has_key);
    assert_eq!(cfg.resolved_base_url(), None);
}

#[test]
fn resolved_base_url_ignores_blank_override() {
    let cfg = SearchConfig {
        backend: SearchBackend::SearxNg,
        base_url: Some("   ".into()),
        ..SearchConfig::default()
    };
    assert_eq!(cfg.resolved_base_url(), None);
}

#[test]
fn resolved_base_url_returns_configured_endpoint() {
    let cfg = SearchConfig {
        backend: SearchBackend::SearxNg,
        base_url: Some("https://searx.example.org".into()),
        ..SearchConfig::default()
    };
    assert_eq!(cfg.resolved_base_url(), Some("https://searx.example.org"));
}

#[test]
fn only_hosted_backends_require_a_key() {
    assert!(!SearchBackend::Tavily.requires_key());
    assert!(!SearchBackend::SearxNg.requires_key());
    assert!(SearchBackend::Brave.requires_key());
    assert!(SearchBackend::OpenAiCompatible.requires_key());
}

#[test]
fn config_round_trips_through_json_without_secrets() {
    let cfg = SearchConfig::default();
    let json = serde_json::to_string(&cfg).unwrap();
    assert!(!json.contains("baseUrl"), "None base_url is skipped");
    assert!(json.contains("hasKey"));
    assert!(
        !json.contains("key\":\""),
        "no secret material in the contract"
    );
    let back: SearchConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(cfg, back);
}

#[test]
fn backend_deserializes_from_camel_case() {
    let b: SearchBackend = serde_json::from_str("\"tavily\"").unwrap();
    assert_eq!(b, SearchBackend::Tavily);
    let b: SearchBackend = serde_json::from_str("\"searxNg\"").unwrap();
    assert_eq!(b, SearchBackend::SearxNg);
    let b: SearchBackend = serde_json::from_str("\"brave\"").unwrap();
    assert_eq!(b, SearchBackend::Brave);
    let b: SearchBackend = serde_json::from_str("\"openAiCompatible\"").unwrap();
    assert_eq!(b, SearchBackend::OpenAiCompatible);
}
