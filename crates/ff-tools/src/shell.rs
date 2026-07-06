//! Cross-platform "run an arbitrary command string in a shell" shim.
//!
//! [`crate::bash`] and the background [`crate::process`] manager hand a free-form
//! command string to a shell. Unix uses `$SHELL -c` (falling back to `/bin/sh`);
//! Windows has no `$SHELL`, so it uses PowerShell (`pwsh -Command`) when present,
//! else `cmd.exe /C`. The model is told (the `bash` tool description) that commands
//! run under cmd/PowerShell on Windows, so it can pick cross-platform invocations.

/// Returns `(program, command_flag)` for spawning a shell that runs a single
/// command string passed as the next argument:
/// `Command::new(program).arg(flag).arg(command)`.
pub(crate) fn shell_invocation() -> (String, &'static str) {
    #[cfg(not(windows))]
    {
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        (sh, "-c")
    }
    #[cfg(windows)]
    {
        // Resolving pwsh.exe walks `PATH`; the result is fixed for the life of the
        // process, so cache it -- `process` can spawn many commands.
        use std::sync::OnceLock;
        static SHELL: OnceLock<(String, &'static str)> = OnceLock::new();
        SHELL
            .get_or_init(|| {
                let path = std::env::var("PATH").unwrap_or_default();
                if let Some(pwsh) = find_in_dirs("pwsh.exe", &path) {
                    return (pwsh, "-Command");
                }
                let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string());
                (comspec, "/C")
            })
            .clone()
    }
}

/// First `dir/name` on `path` (split with the platform's `PATH` separator) that is
/// a file. Existence-only: it checks `is_file()`, not executability -- correct for
/// the Windows use (`.exe` is inherently executable), but a future Unix reuse would
/// need an executable-bit check. Platform-agnostic so it is unit-testable on any
/// host, even though only the Windows branch of [`shell_invocation`] calls it.
#[cfg_attr(not(windows), allow(dead_code))]
fn find_in_dirs(name: &str, path: &str) -> Option<String> {
    std::env::split_paths(path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests;
