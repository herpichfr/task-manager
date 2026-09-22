//! Suspends the TUI to run an external editor on a temp file.
//!
//! The event loop owns the one real `Terminal` and passes it in here.
//! An earlier version built a second `Terminal` over the same stdout at
//! the call site; `clear()` on it then failed with "the cursor position
//! could not be read within a normal duration", and because that error
//! propagated before the temp file was read, every edit the user had just
//! written was silently thrown away. Two rules follow from that bug and
//! are load-bearing: the edited text is read into memory *before* any
//! terminal handling, and no terminal-restoration step is allowed to fail
//! the call.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::error::Result;

/// Restores raw mode and the alternate screen on drop, so an early return
/// (or a panic) while the editor is running can never leave the terminal
/// stuck in "cooked" / primary-screen mode.
struct RestoreGuard;

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, EnableBracketedPaste, EnterAlternateScreen);
        let _ = enable_raw_mode();
    }
}

/// Builds the temp file path for one editing session: a name unique to
/// this call, under `dir`, suffixed `.md` so editors that pick a filetype
/// from the extension (nvim included) treat it as markdown.
fn temp_file_path(dir: &Path, unique: u64) -> PathBuf {
    dir.join(format!("tsk-edit-{unique}.md"))
}

/// Deletes the temp file when the edit session ends, on every path --
/// normal return, early error, or unwind. The file may hold content from
/// an encrypted board, so leaving it behind is a disclosure, not litter.
struct TempFileGuard(PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Creates (or truncates) `path` with owner-only (`0600`) permissions on
/// unix; the content may later belong to an encrypted board.
#[cfg(unix)]
fn create_temp_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_temp_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create(true).truncate(true).open(path)
}

/// True if `path` is owner-read/write only (mode `0600`), nothing more.
#[cfg(all(unix, test))]
fn has_owner_only_permissions(path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode();
    Ok(mode & 0o777 == 0o600)
}

/// Decides which program and arguments launch `editor` on `path`. Inside
/// tmux (`tmux` is `Some` -- the caller passes `$TMUX`'s value, whose
/// content is irrelevant, only its presence), the editor runs in a
/// floating `display-popup` window sized to 80% of the terminal instead of
/// taking over the whole screen; outside tmux, it runs directly, exactly
/// as before this feature. Pure and total, so it is unit-tested without a
/// real tmux session -- the popup path itself cannot be exercised
/// headlessly.
///
/// The TUI underneath is still suspended/restored the same way in both
/// branches (see `edit_text`): `tmux display-popup` draws over the
/// existing session without needing the app under it to leave the
/// alternate screen, but this could not be verified empirically against a
/// real popup in this headless environment, and doing so would risk the
/// load-bearing data-loss fix this module exists to protect. Suspending
/// unconditionally is the same behaviour the fallback path always had, so
/// it is the safe default here too.
pub fn editor_invocation(editor: &str, path: &Path, tmux: Option<&str>) -> (String, Vec<String>) {
    let path_str = path.to_string_lossy().to_string();
    match tmux {
        Some(_) => (
            "tmux".to_string(),
            vec![
                "display-popup".to_string(),
                "-E".to_string(),
                "-w".to_string(),
                "80%".to_string(),
                "-h".to_string(),
                "80%".to_string(),
                editor.to_string(),
                path_str,
            ],
        ),
        None => (editor.to_string(), vec![path_str]),
    }
}

/// Suspends the TUI, runs `editor` on a temp file seeded with `initial`,
/// restores the TUI, and returns the edited contents.
///
/// Sequence, exactly: disable raw mode, leave the alternate screen, disable
/// bracketed paste; spawn the editor (directly, or -- inside tmux -- inside
/// a floating `tmux display-popup`, see `editor_invocation`) and wait for
/// it; re-enable bracketed paste, re-enter the alternate screen, re-enable
/// raw mode, clear the terminal. A non-zero exit (or a spawn failure)
/// discards the edit: `initial` is returned unchanged, not an error.
pub fn edit_text(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    editor: &str,
    initial: &str,
) -> Result<String> {
    let dir = std::env::temp_dir();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ u64::from(std::process::id());
    let path = temp_file_path(&dir, unique);

    {
        let mut file = create_temp_file(&path)?;
        file.write_all(initial.as_bytes())?;
        file.flush()?;
    }
    // From here on the file is removed however we leave this function.
    let _temp = TempFileGuard(path.clone());

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, DisableBracketedPaste)?;
    let guard = RestoreGuard;

    let tmux = std::env::var("TMUX").ok();
    let (program, args) = editor_invocation(editor, &path, tmux.as_deref());
    let status = Command::new(&program).args(&args).status();

    // Read the user's work FIRST. Nothing about restoring the terminal is
    // allowed to stand between the editor exiting and the text being safe
    // in memory -- that ordering is what the cursor-position bug broke.
    let edited = match status {
        Ok(s) if s.success() => fs::read_to_string(&path).unwrap_or_else(|_| initial.to_string()),
        // A non-zero exit means the user abandoned the edit.
        _ => initial.to_string(),
    };

    drop(guard);
    // Cosmetic only: the next draw repaints the screen anyway. This must
    // never discard `edited`.
    let _ = terminal.clear();

    Ok(edited)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_file_path_has_md_suffix_and_is_under_dir() {
        let dir = std::env::temp_dir();
        let p = temp_file_path(&dir, 42);
        assert!(p.starts_with(&dir));
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("md"));
        assert!(p.file_name().unwrap().to_str().unwrap().contains("42"));
    }

    #[test]
    fn temp_file_path_differs_for_different_unique_values() {
        let dir = std::env::temp_dir();
        assert_ne!(temp_file_path(&dir, 1), temp_file_path(&dir, 2));
    }

    #[cfg(unix)]
    #[test]
    fn created_temp_file_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("perm-check.md");
        let _ = create_temp_file(&path).unwrap();
        assert!(has_owner_only_permissions(&path).unwrap());
    }

    /// Regression: the temp file must be gone however the edit session
    /// ends -- it can hold plaintext from an encrypted board.
    #[test]
    fn temp_file_guard_removes_the_file_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leak-check.md");
        std::fs::write(&path, "secret").unwrap();
        assert!(path.exists());
        {
            let _g = TempFileGuard(path.clone());
        }
        assert!(!path.exists(), "temp file survived the guard");
    }

    /// Regression: an early return must still clean up.
    #[test]
    fn temp_file_guard_removes_the_file_when_unwinding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unwind-check.md");
        std::fs::write(&path, "secret").unwrap();
        let p2 = path.clone();
        let _ = std::panic::catch_unwind(move || {
            let _g = TempFileGuard(p2);
            panic!("boom");
        });
        assert!(!path.exists(), "temp file survived an unwind");
    }

    #[cfg(unix)]
    #[test]
    fn create_temp_file_truncates_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trunc.md");
        std::fs::write(&path, "old content that is long").unwrap();
        let _ = create_temp_file(&path).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "");
    }

    // --- editor_invocation: pure, so the tmux-popup branch is testable
    // without a real tmux session ---------------------------------------

    #[test]
    fn editor_invocation_wraps_in_tmux_popup_when_tmux_is_set() {
        let (program, args) = editor_invocation("nvim", Path::new("/tmp/x.md"), Some("/tmp/tmux-1000/default,1234,0"));
        assert_eq!(program, "tmux");
        assert_eq!(
            args,
            vec!["display-popup", "-E", "-w", "80%", "-h", "80%", "nvim", "/tmp/x.md"]
        );
    }

    #[test]
    fn editor_invocation_runs_editor_directly_without_tmux() {
        let (program, args) = editor_invocation("nvim", Path::new("/tmp/x.md"), None);
        assert_eq!(program, "nvim");
        assert_eq!(args, vec!["/tmp/x.md".to_string()]);
    }

    #[test]
    fn editor_invocation_ignores_tmux_env_content_only_presence_matters() {
        let (a, _) = editor_invocation("vim", Path::new("/p"), Some(""));
        let (b, _) = editor_invocation("vim", Path::new("/p"), Some("anything"));
        assert_eq!(a, "tmux");
        assert_eq!(b, "tmux");
    }
}
