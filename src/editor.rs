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
use std::process::{Command, Stdio};
use std::time::Duration;

use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::error::Result;

/// Restores raw mode and the alternate screen on drop, so an early return
/// (or a panic) while the editor is running can never leave the terminal
/// stuck in "cooked" / primary-screen mode. Only ever constructed after the
/// terminal has actually been left (the outside-tmux path); the tmux-popup
/// path never leaves the terminal in the first place, so it never builds
/// one -- see `edit_text`.
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

/// Whether the calling terminal must be suspended (raw mode disabled, the
/// alternate screen left) before the editor runs. `tmux` is `Some` exactly
/// when the caller passes `$TMUX`'s value (its content is irrelevant, only
/// its presence). Outside tmux the editor takes over the whole screen, so
/// the terminal must be suspended first; inside tmux the editor runs in a
/// floating `display-popup` with its own pty, which draws over the existing
/// session without the app underneath ever leaving the alternate screen, so
/// no suspend is needed. Pure and total, so this decision is unit-tested
/// without a real tmux session.
pub fn needs_suspend(tmux: Option<&str>) -> bool {
    tmux.is_none()
}

/// Decides which program and arguments launch `editor` on `path`. Inside
/// tmux (`tmux` is `Some`), the editor runs in a floating `display-popup`
/// window sized to 80% of the terminal, titled with `title` (falling back
/// to "body" when none is given), instead of taking over the whole screen;
/// outside tmux, it runs directly, exactly as before this feature. Pure and
/// total, so the popup path's argument list is unit-tested without a real
/// tmux session -- the popup itself cannot be exercised headlessly.
pub fn editor_invocation(editor: &str, path: &Path, tmux: Option<&str>, title: Option<&str>) -> (String, Vec<String>) {
    let path_str = path.to_string_lossy().to_string();
    match tmux {
        Some(_) => {
            let popup_title = title.unwrap_or("body");
            (
                "tmux".to_string(),
                vec![
                    "display-popup".to_string(),
                    "-E".to_string(),
                    "-w".to_string(),
                    "80%".to_string(),
                    "-h".to_string(),
                    "80%".to_string(),
                    "-T".to_string(),
                    format!(" {popup_title} "),
                    editor.to_string(),
                    path_str,
                ],
            )
        }
        None => (editor.to_string(), vec![path_str]),
    }
}

/// Suspends the TUI, runs `editor` on a temp file seeded with `initial`,
/// restores the TUI, and returns the edited contents. `title` labels the
/// tmux popup's border when the editor runs inside one; it is ignored
/// outside tmux.
///
/// Outside tmux (`needs_suspend` is true): disable raw mode, leave the
/// alternate screen, disable bracketed paste; spawn the editor and wait for
/// it; re-enable bracketed paste, re-enter the alternate screen, re-enable
/// raw mode, clear the terminal -- exactly as before this feature.
///
/// Inside tmux (`needs_suspend` is false): the terminal is left untouched --
/// no raw-mode toggle, no alternate-screen switch -- so the board stays
/// drawn underneath the floating `tmux display-popup`. The `tmux` client's
/// own stdio is set to `/dev/null` so it cannot write into this pane. After
/// it exits, any crossterm input events that queued up while the popup had
/// focus are drained (non-blocking) so none of them are later misread as a
/// key press against the board; the terminal is not cleared, since nothing
/// here ever altered what is already on screen and clearing it would flash
/// a blank frame before the next draw.
///
/// A non-zero exit (or a spawn failure) discards the edit in both cases:
/// `initial` is returned unchanged, not an error.
pub fn edit_text(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    editor: &str,
    initial: &str,
    title: Option<&str>,
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

    let tmux = std::env::var("TMUX").ok();
    let suspend = needs_suspend(tmux.as_deref());

    // `_guard`, once built, restores raw mode / the alternate screen on
    // drop -- covering an early return or a panic, not just the ordinary
    // path. It is only built when `suspend` is true: the tmux-popup path
    // never leaves the terminal in the first place, so it has nothing to
    // restore.
    let _guard = if suspend {
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen, DisableBracketedPaste)?;
        Some(RestoreGuard)
    } else {
        None
    };

    let (program, args) = editor_invocation(editor, &path, tmux.as_deref(), title);
    let mut command = Command::new(&program);
    command.args(&args);
    if !suspend {
        // The tmux popup is its own pty; the `tmux` client itself must not
        // write into (or read from) our pane while it runs.
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    }
    let status = command.status();

    // Read the user's work FIRST. Nothing about restoring the terminal is
    // allowed to stand between the editor exiting and the text being safe
    // in memory -- that ordering is what the cursor-position bug broke.
    let edited = match status {
        Ok(s) if s.success() => fs::read_to_string(&path).unwrap_or_else(|_| initial.to_string()),
        // A non-zero exit means the user abandoned the edit.
        _ => initial.to_string(),
    };

    drop(_guard);

    if !suspend {
        // Drain whatever crossterm input queued up while the popup had
        // focus, so none of it lands on the board as a stray key press.
        // Non-blocking: this must never hang waiting for input that never
        // arrives.
        while event::poll(Duration::ZERO).unwrap_or(false) {
            if event::read().is_err() {
                break;
            }
        }
    } else {
        // Cosmetic only: the next draw repaints the screen anyway. This
        // must never discard `edited`. Skipped in the tmux-popup path,
        // where the terminal was never altered and clearing it would only
        // flash a blank frame before the next draw fills it back in.
        let _ = terminal.clear();
    }

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

    // --- needs_suspend: pure, so this is testable without a real tmux
    // session or a real terminal ------------------------------------------

    #[test]
    fn needs_suspend_is_true_outside_tmux() {
        assert!(needs_suspend(None));
    }

    #[test]
    fn needs_suspend_is_false_inside_tmux() {
        assert!(!needs_suspend(Some("/tmp/tmux-1000/default,1234,0")));
    }

    // --- editor_invocation: pure, so the tmux-popup branch is testable
    // without a real tmux session ---------------------------------------

    #[test]
    fn editor_invocation_wraps_in_tmux_popup_when_tmux_is_set() {
        let (program, args) =
            editor_invocation("nvim", Path::new("/tmp/x.md"), Some("/tmp/tmux-1000/default,1234,0"), None);
        assert_eq!(program, "tmux");
        assert_eq!(
            args,
            vec!["display-popup", "-E", "-w", "80%", "-h", "80%", "-T", " body ", "nvim", "/tmp/x.md"]
        );
    }

    #[test]
    fn editor_invocation_runs_editor_directly_without_tmux() {
        let (program, args) = editor_invocation("nvim", Path::new("/tmp/x.md"), None, None);
        assert_eq!(program, "nvim");
        assert_eq!(args, vec!["/tmp/x.md".to_string()]);
    }

    #[test]
    fn editor_invocation_ignores_tmux_env_content_only_presence_matters() {
        let (a, _) = editor_invocation("vim", Path::new("/p"), Some(""), None);
        let (b, _) = editor_invocation("vim", Path::new("/p"), Some("anything"), None);
        assert_eq!(a, "tmux");
        assert_eq!(b, "tmux");
    }

    #[test]
    fn editor_invocation_uses_the_given_title_in_the_tmux_popup() {
        let (_, args) =
            editor_invocation("nvim", Path::new("/tmp/x.md"), Some("sess"), Some("Buy milk"));
        assert_eq!(
            args,
            vec!["display-popup", "-E", "-w", "80%", "-h", "80%", "-T", " Buy milk ", "nvim", "/tmp/x.md"]
        );
    }

    #[test]
    fn editor_invocation_without_tmux_ignores_title() {
        let (_, args) = editor_invocation("nvim", Path::new("/tmp/x.md"), None, Some("Buy milk"));
        assert_eq!(args, vec!["/tmp/x.md".to_string()]);
    }
}
