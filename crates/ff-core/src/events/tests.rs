use super::*;

#[test]
fn update_progress_serializes_with_exact_contract_keys() {
    let ev = UpdateProgressEvent {
        downloaded: 1024,
        total: Some(4096),
    };
    // The frontend listener reads these exact keys; both are single words so
    // camelCase leaves them unchanged.
    assert_eq!(
        serde_json::to_value(&ev).unwrap(),
        serde_json::json!({ "downloaded": 1024, "total": 4096 })
    );
    // An absent content length serializes as null, not a missing key.
    let unknown = UpdateProgressEvent {
        downloaded: 1024,
        total: None,
    };
    assert_eq!(
        serde_json::to_value(&unknown).unwrap(),
        serde_json::json!({ "downloaded": 1024, "total": null })
    );
}

#[test]
fn phenotype_mcp_unavailable_serializes_with_exact_contract_keys() {
    let ev = PhenotypeMcpUnavailableEvent {
        phenotype: "codon".into(),
        servers: vec!["codegraph".into(), "fetch".into()],
    };
    let v = serde_json::to_value(&ev).unwrap();
    // The frontend listener (lib/ipc.ts onPhenotypeMcpUnavailable) reads these
    // exact keys; both are single words so camelCase leaves them unchanged.
    assert_eq!(
        v,
        serde_json::json!({ "phenotype": "codon", "servers": ["codegraph", "fetch"] })
    );
    let back: PhenotypeMcpUnavailableEvent = serde_json::from_value(v).unwrap();
    assert_eq!(back.phenotype, "codon");
    assert_eq!(back.servers, vec!["codegraph", "fetch"]);
}
