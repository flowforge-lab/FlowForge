use super::*;

#[test]
fn workspace_keys_with_same_path_are_equal() {
    let tmp = std::env::temp_dir();
    let a = InstanceKey::workspace("codegraph", &tmp);
    let b = InstanceKey::workspace("codegraph", &tmp);
    assert_eq!(a, b);
}

#[test]
fn workspace_keys_with_different_paths_differ() {
    let a = InstanceKey::workspace("codegraph", Path::new("/tmp/aaa-ff-test"));
    let b = InstanceKey::workspace("codegraph", Path::new("/tmp/bbb-ff-test"));
    assert_ne!(a, b);
}

#[test]
fn global_and_workspace_keys_differ() {
    let g = InstanceKey::global("codegraph");
    let w = InstanceKey::workspace("codegraph", Path::new("/tmp/ff-test"));
    assert_ne!(g, w);
    assert_eq!(g.scope.display(), None);
    assert_eq!(w.scope.display(), Some("/tmp/ff-test".to_string()));
}

#[test]
fn canonicalization_collapses_dot_segments() {
    let tmp = std::env::temp_dir();
    let dotted = tmp.join(".");
    let a = ScopeKey::workspace(&tmp);
    let b = ScopeKey::workspace(&dotted);
    assert_eq!(a, b);
}
