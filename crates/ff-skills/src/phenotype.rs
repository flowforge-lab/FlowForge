//! Loading phenotype definitions from `~/.flowforge/phenos/<name>.toml` (RFC 0001 §7).
//!
//! A [`Phenotype`](ff_core::Phenotype) is a named, switchable working set: which skills
//! are active plus optional model and persona overrides. Definitions are user-authored
//! TOML files; [`load_phenotypes`] scans a directory into a name-sorted map. A built-in
//! [`default_phenotype`] (no skills, no overrides) always exists so the app has a valid
//! selection even with an empty or missing directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ff_core::Phenotype;

/// The reserved name of the built-in phenotype.
pub const DEFAULT_PHENOTYPE: &str = "default";

/// Why a single phenotype file failed to load. Collected per-file by
/// [`load_phenotypes`] rather than failing the whole scan — one broken file must
/// not hide the rest.
#[derive(Debug, thiserror::Error)]
pub enum PhenotypeError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: invalid phenotype TOML: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{name:?} is not a valid phenotype name (must be a single safe file stem)")]
    InvalidName { name: String },
    #[error("the built-in {name:?} phenotype is immutable and cannot be saved")]
    Immutable { name: String },
    #[error("{name}: failed to serialize phenotype: {source}")]
    Serialize {
        name: String,
        #[source]
        source: toml::ser::Error,
    },
}

/// The built-in phenotype: the default working set with the built-in tools and no
/// skills (RFC 0001 §7). Always available, never read from disk.
pub fn default_phenotype() -> Phenotype {
    Phenotype {
        name: DEFAULT_PHENOTYPE.to_string(),
        skills: Vec::new(),
        model: None,
        persona: None,
        max_iterations: None,
        provider: None,
    }
}

/// On-disk shape of a phenotype file. `name` is intentionally absent: the file stem
/// is authoritative, so the TOML never sets it. Mirrors the fields of
/// [`Phenotype`](ff_core::Phenotype) that users author.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PhenotypeFile {
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    persona: Option<String>,
    #[serde(default)]
    max_iterations: Option<usize>,
    #[serde(default)]
    provider: Option<String>,
}

impl PhenotypeFile {
    fn into_phenotype(self, name: String) -> Phenotype {
        Phenotype {
            name,
            skills: self.skills,
            model: self.model,
            persona: self.persona,
            max_iterations: self.max_iterations,
            provider: self.provider,
        }
    }
}

/// Load every `<root>/<name>.toml` into a name-sorted map. Resilient: a file that
/// fails to read or parse is skipped and its error collected. The file stem is the
/// authoritative phenotype name, so a `name` field in the TOML is overridden by the
/// filename (keeps the on-disk identity unambiguous). A missing directory is a
/// normal first-run state, not an error.
pub fn load_phenotypes(root: &Path) -> (BTreeMap<String, Phenotype>, Vec<PhenotypeError>) {
    let mut out = BTreeMap::new();
    let mut errors = Vec::new();

    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return (out, errors),
    };

    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();

    for path in files {
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(source) => {
                errors.push(PhenotypeError::Io { path, source });
                continue;
            }
        };
        match toml::from_str::<PhenotypeFile>(&text) {
            Ok(file) => {
                out.insert(stem.clone(), file.into_phenotype(stem));
            }
            Err(source) => errors.push(PhenotypeError::Parse { path, source }),
        }
    }

    (out, errors)
}

/// On-disk serialize shape for a phenotype. Mirrors [`PhenotypeFile`] but omits
/// `None`/empty fields so saved files stay minimal, and never writes `name` (the
/// file stem is authoritative — see [`load_phenotypes`]).
#[derive(serde::Serialize)]
struct PhenotypeOut {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    skills: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persona: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_iterations: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
}

/// Whether `name` is safe to use as a single-segment file stem. Rejects empty
/// names, path separators, and leading dots so a phenotype name can never escape
/// the phenotypes directory (path-traversal guard).
fn is_valid_stem(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains('/') && !name.contains('\\')
}

/// Persist `pheno` to `<root>/<name>.toml`, creating `root` if needed. Upsert:
/// overwrites any existing file with the same name. The built-in
/// [`DEFAULT_PHENOTYPE`] is immutable and rejected (RFC 0005 Phase D / #525), as
/// are names that are not safe file stems. The write is atomic — serialized to a
/// sibling `.tmp` then renamed into place — so a crash mid-write can never leave a
/// truncated file.
pub fn save_phenotype(root: &Path, pheno: &Phenotype) -> Result<(), PhenotypeError> {
    if pheno.name == DEFAULT_PHENOTYPE {
        return Err(PhenotypeError::Immutable {
            name: pheno.name.clone(),
        });
    }
    if !is_valid_stem(&pheno.name) {
        return Err(PhenotypeError::InvalidName {
            name: pheno.name.clone(),
        });
    }

    let out = PhenotypeOut {
        skills: pheno.skills.clone(),
        model: pheno.model.clone(),
        persona: pheno.persona.clone(),
        max_iterations: pheno.max_iterations,
        provider: pheno.provider.clone(),
    };
    let body = toml::to_string_pretty(&out).map_err(|source| PhenotypeError::Serialize {
        name: pheno.name.clone(),
        source,
    })?;

    std::fs::create_dir_all(root).map_err(|source| PhenotypeError::Io {
        path: root.to_path_buf(),
        source,
    })?;

    let final_path = root.join(format!("{}.toml", pheno.name));
    let tmp_path = root.join(format!("{}.toml.tmp", pheno.name));
    std::fs::write(&tmp_path, body).map_err(|source| PhenotypeError::Io {
        path: tmp_path.clone(),
        source,
    })?;
    std::fs::rename(&tmp_path, &final_path).map_err(|source| PhenotypeError::Io {
        path: final_path,
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn default_has_no_skills_or_overrides() {
        let d = default_phenotype();
        assert_eq!(d.name, DEFAULT_PHENOTYPE);
        assert!(d.skills.is_empty());
        assert!(d.model.is_none());
        assert!(d.persona.is_none());
    }

    #[test]
    fn loads_valid_phenotype() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "rust.toml",
            "skills = [\"cargo-help\", \"clippy\"]\nmodel = \"qwen3-coder\"\npersona = \"You are a Rust expert.\"\n",
        );
        let (map, errs) = load_phenotypes(dir.path());
        assert!(errs.is_empty(), "{errs:?}");
        let p = map.get("rust").expect("rust phenotype");
        assert_eq!(p.name, "rust");
        assert_eq!(p.skills, vec!["cargo-help", "clippy"]);
        assert_eq!(p.model.as_deref(), Some("qwen3-coder"));
        assert_eq!(p.persona.as_deref(), Some("You are a Rust expert."));
    }

    #[test]
    fn loads_max_iterations_override() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "codon.toml",
            "skills = []
max_iterations = 40
",
        );
        write(
            dir.path(),
            "plain.toml",
            "skills = []
",
        );
        let (map, errs) = load_phenotypes(dir.path());
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(map["codon"].max_iterations, Some(40));
        assert_eq!(map["plain"].max_iterations, None);
    }

    #[test]
    fn name_comes_from_filename_not_toml() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "writing.toml", "skills = []\n");
        let (map, errs) = load_phenotypes(dir.path());
        assert!(errs.is_empty(), "{errs:?}");
        assert!(map.contains_key("writing"));
        assert_eq!(map["writing"].name, "writing");
    }

    #[test]
    fn malformed_file_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "good.toml", "skills = []\n");
        write(dir.path(), "bad.toml", "skills = \"not a list\"\n");
        let (map, errs) = load_phenotypes(dir.path());
        assert!(map.contains_key("good"));
        assert!(!map.contains_key("bad"));
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn missing_dir_is_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let (map, errs) = load_phenotypes(&dir.path().join("nope"));
        assert!(map.is_empty());
        assert!(errs.is_empty());
    }

    #[test]
    fn non_toml_files_ignored() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "notes.md", "not a phenotype");
        write(dir.path(), "rust.toml", "skills = []\n");
        let (map, _) = load_phenotypes(dir.path());
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("rust"));
    }

    fn pheno(name: &str) -> Phenotype {
        Phenotype {
            name: name.to_string(),
            skills: vec!["cargo-help".into()],
            model: Some("qwen3-coder".into()),
            persona: Some("Rust expert.".into()),
            max_iterations: Some(40),
            provider: Some("local-ollama".into()),
        }
    }

    #[test]
    fn save_round_trips_via_load() {
        let dir = tempfile::tempdir().unwrap();
        save_phenotype(dir.path(), &pheno("rust")).unwrap();
        let (map, errs) = load_phenotypes(dir.path());
        assert!(errs.is_empty(), "{errs:?}");
        let p = map.get("rust").expect("rust phenotype");
        assert_eq!(p, &pheno("rust"));
    }

    #[test]
    fn save_omits_name_and_none_fields() {
        let dir = tempfile::tempdir().unwrap();
        let bare = Phenotype {
            name: "bare".into(),
            skills: Vec::new(),
            model: None,
            persona: None,
            max_iterations: None,
            provider: None,
        };
        save_phenotype(dir.path(), &bare).unwrap();
        let body = fs::read_to_string(dir.path().join("bare.toml")).unwrap();
        assert!(!body.contains("name"), "name must not be written: {body:?}");
        assert!(!body.contains("model"), "None fields omitted: {body:?}");
        assert!(!body.contains("provider"), "None fields omitted: {body:?}");
    }

    #[test]
    fn save_refuses_default() {
        let dir = tempfile::tempdir().unwrap();
        let err = save_phenotype(dir.path(), &default_phenotype()).unwrap_err();
        assert!(matches!(err, PhenotypeError::Immutable { .. }));
        assert!(!dir.path().join("default.toml").exists());
    }

    #[test]
    fn save_rejects_unsafe_names() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["../evil", "a/b", "", ".hidden", "a\\b"] {
            let err = save_phenotype(dir.path(), &pheno(bad)).unwrap_err();
            assert!(
                matches!(err, PhenotypeError::InvalidName { .. }),
                "{bad:?} should be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn save_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        save_phenotype(dir.path(), &pheno("rust")).unwrap();
        let mut updated = pheno("rust");
        updated.model = Some("qwen3-max".into());
        save_phenotype(dir.path(), &updated).unwrap();
        let (map, _) = load_phenotypes(dir.path());
        assert_eq!(map["rust"].model.as_deref(), Some("qwen3-max"));
    }

    #[test]
    fn save_leaves_no_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        save_phenotype(dir.path(), &pheno("rust")).unwrap();
        assert!(!dir.path().join("rust.toml.tmp").exists());
        assert!(dir.path().join("rust.toml").exists());
    }

    #[test]
    fn save_creates_missing_root_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("phenos");
        save_phenotype(&root, &pheno("rust")).unwrap();
        assert!(root.join("rust.toml").exists());
    }
}
