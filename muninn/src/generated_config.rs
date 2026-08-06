//! Writing the generated Telegraf configuration.
//!
//! One writer, one permission rule. The file can hold resolved secrets — the
//! supervisor's copy always does, and `render-config --output` does whenever
//! `--unsafe-show-secrets` is set — so it is created owner-only on Unix.
//!
//! The mode is set **at creation** rather than with a `set_permissions` call
//! afterwards. Writing first and restricting second leaves a window in which the
//! file exists with the process umask's mode and already contains the token. In
//! the shipped deployment `/run/muninn` is 0700 and nobody else can reach into
//! it, so the window is not reachable there; `render-config --output` writes
//! wherever the operator points it, which is where it would be.

use std::path::Path;

use muninn_core::{MuninnError, Result};

/// Write `contents` to `path`, creating its directory, owner-readable only.
pub(crate) fn write(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        // An empty parent is what `Path::parent` returns for a bare filename;
        // creating "" fails, and there is nothing to create.
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| {
                MuninnError::internal(format!("cannot create '{}': {e}", dir.display()))
            })?;
        }
    }

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    let mut file = options
        .open(path)
        .map_err(|e| MuninnError::internal(format!("cannot write '{}': {e}", path.display())))?;

    use std::io::Write as _;
    file.write_all(contents.as_bytes())
        .map_err(|e| MuninnError::internal(format!("cannot write '{}': {e}", path.display())))?;

    // `mode` only applies when the file is created. An existing file keeps the
    // mode it had, which on a restart into the same tmpfs path would silently
    // inherit whatever was there before.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            MuninnError::internal(format!(
                "cannot restrict permissions on '{}': {e}",
                path.display()
            ))
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_the_directory_it_needs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/telegraf.conf");

        write(&path, "[agent]\n").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[agent]\n");
    }

    #[test]
    fn truncates_rather_than_appending() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegraf.conf");

        write(&path, "first, and longer\n").unwrap();
        write(&path, "second\n").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
    }

    /// The file holds resolved secrets. A world-readable one on a shared tmpfs
    /// undoes the reason it is not persisted in the first place.
    #[cfg(unix)]
    #[test]
    fn is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegraf.conf");

        write(&path, "token = \"secret\"\n").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }

    /// Rewriting a path that already exists with a looser mode must restrict it
    /// again — `OpenOptions::mode` alone would not, because it applies to
    /// creation only.
    #[cfg(unix)]
    #[test]
    fn restricts_a_file_that_already_existed() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegraf.conf");
        std::fs::write(&path, "stale\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write(&path, "token = \"secret\"\n").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }
}
