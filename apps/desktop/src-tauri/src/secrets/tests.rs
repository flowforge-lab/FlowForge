use super::*;

#[test]
fn set_get_clear_roundtrip() {
    let id = "conn-secrets-roundtrip";
    assert!(get(id, SecretKind::ApiKey).is_none());
    set(id, SecretKind::ApiKey, "sk-123").unwrap();
    assert_eq!(get(id, SecretKind::ApiKey).as_deref(), Some("sk-123"));
    clear(id, SecretKind::ApiKey).unwrap();
    assert!(get(id, SecretKind::ApiKey).is_none());
}

#[test]
fn clearing_absent_secret_is_ok() {
    clear("conn-secrets-absent", SecretKind::SessionToken).unwrap();
}

#[test]
fn present_reflects_stored_kinds_in_all_order() {
    let id = "conn-secrets-present";
    assert!(present(id).is_empty());
    set(id, SecretKind::SessionToken, "tok").unwrap();
    set(id, SecretKind::ApiKey, "sk").unwrap();
    assert_eq!(
        present(id),
        vec![SecretKind::ApiKey, SecretKind::SessionToken]
    );
    clear(id, SecretKind::ApiKey).unwrap();
    assert_eq!(present(id), vec![SecretKind::SessionToken]);
}

#[test]
fn kinds_are_isolated_per_connection() {
    let id = "conn-secrets-isolated";
    set(id, SecretKind::SecretAccessKey, "aws-secret").unwrap();
    set(id, SecretKind::SessionToken, "aws-token").unwrap();
    assert_eq!(
        get(id, SecretKind::SecretAccessKey).as_deref(),
        Some("aws-secret")
    );
    assert_eq!(
        get(id, SecretKind::SessionToken).as_deref(),
        Some("aws-token")
    );
    assert!(get(id, SecretKind::ApiKey).is_none());
}
