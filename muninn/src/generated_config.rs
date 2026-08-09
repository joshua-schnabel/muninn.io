//! Writing the generated Telegraf configuration.
//!
//! One writer, one permission rule. The file can hold resolved secrets — the
//! supervisor's copy always does, and `render-config --output` does whenever
//! `--unsafe-show-secrets` is set — so it is created owner-only on Unix and
//! never exists at the target path in any other state.
//!
//! # Write to a fresh name, then rename
//!
//! Opening the target path with `create(true).truncate(true)` was the obvious
//! implementation and had two problems, neither reachable in the shipped
//! deployment — `/run/muninn` is 0700 and nobody else can reach into it — but
//! both reachable through `runtime.generated_config_path`, which is
//! configurable, and through `render-config --output`, which writes wherever
//! the operator points it. Running muninn directly on a host is supported, and
//! there the directory is not necessarily anyone's private tmpfs (F-09).
//!
//! * **`open` follows a symlink.** A pre-placed link at the target path would
//!   have redirected a file containing resolved credentials to wherever it
//!   pointed. `rename` does not follow one: it replaces the link itself.
//! * **Truncate-then-write is not atomic.** A reader — Telegraf, on a restart —
//!   can observe an empty or half-written configuration. `rename` is atomic, so
//!   the path holds either the old contents or the new ones and never a
//!   fragment.
//!
//! The temporary name is random rather than derived from the process id, so
//! nothing can be pre-placed at the name muninn is about to use. It is created
//! by `mkstemp`, which is `O_EXCL` and owner-only from the first byte, so the
//! mode is right at creation rather than restored afterwards — writing first
//! and restricting second leaves a window in which the file exists with the
//! umask's mode and already contains the token.

use std::path::Path;

use muninn_core::{MuninnError, Result};

/// Write `contents` to `path`, creating its directory, owner-readable only.
pub(crate) fn write(path: &Path, contents: &str) -> Result<()> {
    let dir = match path.parent() {
        // An empty parent is what `Path::parent` returns for a bare filename:
        // the target is in the current directory, and there is nothing to
        // create.
        Some(d) if !d.as_os_str().is_empty() => {
            std::fs::create_dir_all(d).map_err(|e| {
                MuninnError::internal(format!("cannot create '{}': {e}", d.display()))
            })?;
            d
        }
        _ => Path::new("."),
    };

    // In the same directory as the target, or `persist` below would be a
    // cross-device rename and fail.
    let mut file = tempfile::Builder::new()
        .prefix(".muninn-telegraf-")
        .suffix(".conf")
        .tempfile_in(dir)
        .map_err(|e| {
            MuninnError::internal(format!(
                "cannot create a temporary file in '{}': {e}",
                dir.display()
            ))
        })?;

    // `mkstemp` already creates 0600; asserting it costs nothing and means the
    // guarantee does not rest on a property of the dependency.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| {
                MuninnError::internal(format!("cannot restrict the temporary file: {e}"))
            })?;
    }

    use std::io::Write as _;
    file.write_all(contents.as_bytes())
        .map_err(|e| MuninnError::internal(format!("cannot write '{}': {e}", path.display())))?;
    // Before the rename: a rename that beats its own contents to disk would
    // leave an empty configuration at the target after a power loss.
    file.as_file()
        .sync_all()
        .map_err(|e| MuninnError::internal(format!("cannot flush '{}': {e}", path.display())))?;

    file.persist(path).map_err(|e| {
        // `persist` returns the temporary file with the error, and dropping it
        // removes it — so a failed rename leaves nothing holding credentials
        // behind.
        MuninnError::internal(format!("cannot write '{}': {}", path.display(), e.error))
    })?;

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

    /// Rewriting a path that already exists with a looser mode must not inherit
    /// that mode. Writing through a fresh file and renaming makes this hold by
    /// construction — the mode of whatever was there is irrelevant, because
    /// nothing is written into it.
    #[cfg(unix)]
    #[test]
    fn does_not_inherit_the_mode_of_a_file_that_already_existed() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegraf.conf");
        std::fs::write(&path, "stale\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write(&path, "token = \"secret\"\n").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }

    /// A symlink at the target path must be replaced, not followed.
    ///
    /// The finding (F-09): the writer opened the target with
    /// `create(true).truncate(true)`, which follows a link — so anything able
    /// to place one where muninn is about to write could have redirected a file
    /// containing resolved credentials to a path of its choosing. The default
    /// deployment is a 0700 tmpfs, but the path is configurable and
    /// `render-config --output` writes wherever it is told.
    ///
    /// The assertion is that the *decoy is untouched* — asserting only that the
    /// target has the right contents would pass while the link was followed.
    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_target_is_replaced_rather_than_followed() {
        let dir = tempfile::tempdir().unwrap();
        let decoy = dir.path().join("decoy.conf");
        std::fs::write(&decoy, "untouched\n").unwrap();

        let path = dir.path().join("telegraf.conf");
        std::os::unix::fs::symlink(&decoy, &path).unwrap();

        write(&path, "token = \"secret\"\n").unwrap();

        assert_eq!(
            std::fs::read_to_string(&decoy).unwrap(),
            "untouched\n",
            "the write followed the symlink"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "token = \"secret\"\n"
        );
        assert!(
            !std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link should have been replaced by a regular file"
        );
    }

    /// The temporary file is not left behind, whatever its name was.
    ///
    /// A scratch file holding resolved credentials that survives the call would
    /// undo the reason the real one is on a tmpfs.
    #[test]
    fn nothing_is_left_beside_the_file_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegraf.conf");

        write(&path, "token = \"secret\"\n").unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("telegraf.conf")]);
    }
}
