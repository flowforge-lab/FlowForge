//! Materialize an install source (local path / git URL / raw-Markdown URL) into a
//! self-contained temp directory the installer can validate before placement.
//!
//! Network and subprocess I/O is isolated here so the rest of the installer is pure
//! filesystem work over an already-fetched directory.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use super::InstallError;

/// How a source string is interpreted. Local paths win over URL heuristics so a
/// directory literally named like a URL is still treated as a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    LocalDir,
    LocalFile,
    Git,
    Http,
}

/// Classify a source string. Existing paths are local; otherwise git URLs
/// (`git@`, `ssh://`, `.git`) take precedence over plain `http(s)` (raw Markdown).
pub(crate) fn classify(source: &str) -> Result<SourceKind, InstallError> {
    let p = Path::new(source);
    if p.is_dir() {
        return Ok(SourceKind::LocalDir);
    }
    if p.is_file() {
        return Ok(SourceKind::LocalFile);
    }
    if source.starts_with("git@")
        || source.starts_with("ssh://")
        || source.starts_with("file://")
        || source.ends_with(".git")
    {
        return Ok(SourceKind::Git);
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        return Ok(SourceKind::Http);
    }
    // Looks like a path that simply does not exist vs. a wholly unknown scheme.
    if source.contains('/') || source.starts_with('.') {
        Err(InstallError::NotFound(source.to_string()))
    } else {
        Err(InstallError::UnsupportedSource(source.to_string()))
    }
}

/// Fetch `source` into a fresh temp dir. Returns the temp handle (kept alive to
/// own the bytes) and the path to the skill directory inside it, which is
/// guaranteed to exist but not yet validated.
pub(crate) fn materialize(source: &str) -> Result<(TempDir, PathBuf), InstallError> {
    let kind = classify(source)?;
    let temp = TempDir::new().map_err(|e| io("creating temp dir", e))?;
    let skill_dir = temp.path().join("skill");

    match kind {
        SourceKind::LocalDir => copy_tree(Path::new(source), &skill_dir)?,
        SourceKind::LocalFile => {
            let body = std::fs::read_to_string(source).map_err(|e| io("reading source file", e))?;
            write_manifest_only(&skill_dir, &body)?;
        }
        SourceKind::Git => clone_git(source, &skill_dir)?,
        SourceKind::Http => {
            let body = http_get(source)?;
            write_manifest_only(&skill_dir, &body)?;
        }
    }

    Ok((temp, skill_dir))
}

/// Write a single-file source as `<skill_dir>/SKILL.md`.
fn write_manifest_only(skill_dir: &Path, body: &str) -> Result<(), InstallError> {
    std::fs::create_dir_all(skill_dir).map_err(|e| io("creating skill dir", e))?;
    std::fs::write(skill_dir.join("SKILL.md"), body).map_err(|e| io("writing SKILL.md", e))
}

/// Shallow-clone a git source, then strip `.git` so the installed skill is a clean
/// tree (and the executable scan need not special-case hooks).
fn clone_git(url: &str, dest: &Path) -> Result<(), InstallError> {
    if Command::new("git").arg("--version").output().is_err() {
        return Err(InstallError::GitUnavailable);
    }
    let out = Command::new("git")
        // Restrict to ordinary transports so a crafted source can't reach the
        // `ext::`/`fd::` helpers (arbitrary command execution).
        .env("GIT_ALLOW_PROTOCOL", "file:git:http:https:ssh")
        .args(["clone", "--depth", "1", "--quiet", "--", url])
        .arg(dest)
        .output()
        .map_err(|e| InstallError::Fetch {
            url: url.to_string(),
            detail: e.to_string(),
        })?;
    if !out.status.success() {
        return Err(InstallError::Fetch {
            url: url.to_string(),
            detail: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    let git_dir = dest.join(".git");
    if git_dir.exists() {
        let _ = std::fs::remove_dir_all(&git_dir);
    }
    Ok(())
}

/// Largest raw-Markdown body we will buffer (1 MiB). A `SKILL.md` is text; a body
/// past this is almost certainly hostile (memory-exhaustion) rather than a skill.
const MAX_HTTP_BODY: u64 = 1 << 20;

/// Blocking GET of a raw-Markdown source. Caps the response size (memory DoS) and
/// bounds redirects (a fully-open redirect chain widens the SSRF surface for an
/// agent-supplied URL); a private-IP allowlist is left to a later pass.
fn http_get(url: &str) -> Result<String, InstallError> {
    let fetch_err = |e: reqwest::Error| InstallError::Fetch {
        url: url.to_string(),
        detail: e.to_string(),
    };
    let too_large = || InstallError::Fetch {
        url: url.to_string(),
        detail: format!("response exceeds the {MAX_HTTP_BODY}-byte limit"),
    };

    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(fetch_err)?;
    let resp = client
        .get(url)
        .send()
        .map_err(fetch_err)?
        .error_for_status()
        .map_err(fetch_err)?;

    // Trust a declared Content-Length to fail fast, but always enforce the cap on
    // the actual stream (a lying or absent header must not let the body grow).
    if resp.content_length().is_some_and(|len| len > MAX_HTTP_BODY) {
        return Err(too_large());
    }
    let mut buf = Vec::new();
    resp.take(MAX_HTTP_BODY + 1)
        .read_to_end(&mut buf)
        .map_err(|e| InstallError::Fetch {
            url: url.to_string(),
            detail: e.to_string(),
        })?;
    if buf.len() as u64 > MAX_HTTP_BODY {
        return Err(too_large());
    }
    String::from_utf8(buf).map_err(|e| InstallError::Fetch {
        url: url.to_string(),
        detail: format!("response is not valid UTF-8: {e}"),
    })
}

/// Recursively copy `src` into `dest`.
fn copy_tree(src: &Path, dest: &Path) -> Result<(), InstallError> {
    std::fs::create_dir_all(dest).map_err(|e| io("creating skill dir", e))?;
    for entry in std::fs::read_dir(src).map_err(|e| io("reading source dir", e))? {
        let entry = entry.map_err(|e| io("reading source entry", e))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| io("copying file", e))?;
        }
    }
    Ok(())
}

fn io(context: &str, source: std::io::Error) -> InstallError {
    InstallError::Io {
        context: context.to_string(),
        source,
    }
}
