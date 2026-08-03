//! Loading phenotype definitions from `~/.flowforge/phenos/<name>.toml` (RFC 0001 §7).
//!
//! A [`Phenotype`](ff_core::Phenotype) is a named, switchable working set: which skills
//! are active plus optional model and persona overrides. Definitions are user-authored
//! TOML files; [`load_phenotypes`] scans a directory into a name-sorted map. A built-in
//! [`default_phenotype`] (no skills, no overrides) always exists so the app has a valid
//! selection even with an empty or missing directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ff_core::{McpServerConfig, Phenotype};

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
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
        preheat: Vec::new(),
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
    #[serde(default)]
    mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    egress: ff_core::Egress,
    /// #1179 3B. Must be mirrored here as well as on `Phenotype`: this struct is
    /// `deny_unknown_fields`, so omitting it would make a valid `preheat = [...]`
    /// a hard parse error rather than an ignored key.
    #[serde(default)]
    preheat: Vec<String>,
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
            mcp_servers: self.mcp_servers,
            egress: self.egress,
            preheat: self.preheat,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    mcp_servers: Vec<McpServerConfig>,
    /// Network-egress policy (RFC 0013). Omitted when `Open` (the default) so
    /// existing saved files stay byte-identical; written as `egress = "localOnly"`
    /// for a restricted phenotype. Without this, editing+saving a phenotype would
    /// silently reset its egress to `Open` (the read path carries it, but the write
    /// path dropped it before this fix).
    #[serde(skip_serializing_if = "ff_core::Egress::is_open")]
    egress: ff_core::Egress,
    /// Tool names to preheat into the resident block (#1179). Omitted when empty so
    /// files for the common no-preheat case stay minimal. Same trap as `egress`
    /// above: the read path carried this from the start, and without it here,
    /// editing+saving a phenotype silently erased its preheat list.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    preheat: Vec<String>,
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
        mcp_servers: pheno.mcp_servers.clone(),
        egress: pheno.egress,
        preheat: pheno.preheat.clone(),
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
mod tests;
