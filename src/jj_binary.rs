use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use crate::error::{JjPlanError, Result};

/// Resolved jj binary path, cached for the lifetime of the process.
pub struct JjBinary {
    path: PathBuf,
}

impl JjBinary {
    /// Resolve the real jj binary on `$PATH`, skipping ourselves.
    ///
    /// Walks `$PATH` entries looking for an executable `jj` whose canonical
    /// path differs from our own. This mirrors the zsh shim's `SELF`/`REAL_JJ`
    /// resolution logic.
    pub fn resolve() -> Result<Self> {
        let self_exe = std::env::current_exe().map_err(JjPlanError::SelfResolution)?;
        let self_canonical = std::fs::canonicalize(&self_exe).unwrap_or(self_exe);

        let path_var = std::env::var_os("PATH").unwrap_or_default();
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("jj");
            if !candidate.is_file() {
                continue;
            }
            // Skip if this is ourselves (compare canonical paths)
            let candidate_canonical = std::fs::canonicalize(&candidate).unwrap_or(candidate.clone());
            if candidate_canonical == self_canonical {
                continue;
            }
            // Check executable bit on unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = candidate.metadata() {
                    if meta.permissions().mode() & 0o111 == 0 {
                        continue; // not executable
                    }
                } else {
                    continue;
                }
            }
            return Ok(Self {
                path: candidate_canonical,
            });
        }

        Err(JjPlanError::JjBinaryNotFound)
    }

    /// Return the path to the resolved jj binary.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Replace this process with jj (unix exec). Does not return on success.
    ///
    /// This is the zero-overhead passthrough path used for read-only commands.
    #[cfg(unix)]
    pub fn exec(&self, args: &[String]) -> Result<()> {
        use std::os::unix::process::CommandExt;
        let err = Command::new(&self.path).args(args).exec();
        // exec() only returns on error
        Err(JjPlanError::JjExecFailed(err))
    }

    /// Non-unix fallback: spawn jj and exit with its status code.
    #[cfg(not(unix))]
    pub fn exec(&self, args: &[String]) -> Result<()> {
        let status = self.run_status(args)?;
        std::process::exit(status.code().unwrap_or(1));
    }

    /// Run jj as a child process, inheriting stdin/stdout/stderr.
    /// Returns the exit status.
    ///
    /// Used for wrapped (mutating) commands where we need to do work
    /// before and after the jj command.
    pub fn run_inherit(&self, args: &[String]) -> Result<ExitStatus> {
        Command::new(&self.path)
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(JjPlanError::JjExecFailed)
    }

    /// Run jj as a child process, capturing stdout. Stderr is inherited.
    /// Returns (exit_status, stdout_string).
    ///
    /// Used for commands where we need to parse jj's output (e.g. `jj root`).
    pub fn run_capture_stdout(&self, args: &[String]) -> Result<(ExitStatus, String)> {
        let output = Command::new(&self.path)
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .map_err(JjPlanError::JjExecFailed)?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        Ok((output.status, stdout))
    }

    /// Run jj silently, capturing both stdout and stderr.
    /// Returns (exit_status, stdout, stderr).
    ///
    /// Used for probing commands where we don't want to display output
    /// (e.g. checking `jj root` to see if we're in a repo).
    pub fn run_silent(&self, args: &[String]) -> Result<(ExitStatus, String, String)> {
        let output = Command::new(&self.path)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(JjPlanError::JjExecFailed)?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok((output.status, stdout, stderr))
    }

    /// Convenience: get the repo root via `jj root`, or None if not in a repo.
    pub fn repo_root(&self) -> Option<PathBuf> {
        let args = vec!["root".to_string()];
        match self.run_silent(&args) {
            Ok((status, stdout, _)) if status.success() => {
                let trimmed = stdout.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(trimmed))
                }
            }
            _ => None,
        }
    }
}