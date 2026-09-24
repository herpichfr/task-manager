//! Terminal lifecycle (raw mode, alternate screen, panic-safe restore) and
//! the main draw/poll/handle event loop.

use std::io;
use std::panic;
use std::time::Duration;

use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::App;
use crate::error::{AppError, Result};
use crate::ui;

/// Restores the terminal to its normal state. Best-effort: called both on
/// the ordinary exit path and from the panic hook, so failures here are
/// swallowed by callers rather than propagated.
fn restore_terminal() -> io::Result<()> {
    disable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, LeaveAlternateScreen, DisableBracketedPaste)?;
    Ok(())
}

/// Installs a panic hook that restores the terminal before running the
/// previously installed (default) hook, so a crash never leaves the user's
/// terminal (or tmux pane) stuck in raw/alternate-screen mode.
fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        default_hook(info);
    }));
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

/// Runs the TUI to completion. Enters raw mode / the alternate screen,
/// drives the event loop until `app.should_quit`, then restores the
/// terminal on every exit path -- success, error, or panic.
pub fn run(app: &mut App) -> Result<()> {
    install_panic_hook();

    let mut terminal = match setup_terminal() {
        Ok(t) => t,
        Err(e) => {
            let _ = restore_terminal();
            return Err(AppError::Io(e));
        }
    };

    let result = event_loop(&mut terminal, app);
    let _ = restore_terminal();
    result
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::render(frame, app))?;

        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                // Only `Press` is handled: some terminals also deliver
                // `Release`, which would otherwise double-act on one key.
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.handle_key(key);
                    // The editor is run out here, with the loop's own
                    // `Terminal`. `App` only records the request, so it
                    // stays unit-testable with no terminal at all.
                    if let Some((target, initial)) = app.take_pending_edit() {
                        let title = app.pending_edit_title(target);
                        match app.editor_command() {
                            Ok(bin) => match crate::editor::edit_text(terminal, &bin, &initial, title.as_deref()) {
                                Ok(text) => app.apply_edited_text(target, text),
                                Err(e) => app.report_editor_error(e),
                            },
                            Err(e) => app.report_editor_error(e),
                        }
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        // Rate-limited to about once a second inside `App`; called every
        // tick (idle or not) so a database replaced on disk -- e.g. by
        // Dropbox syncing the other machine's write -- is picked up even
        // while a popup or form is open, and any later write goes to the
        // new file instead of an orphaned one.
        app.check_external_change();
        for task in app.unnotified_deadline_tasks() {
            let body = format!("\"{}\" has reached its deadline.", task.title);
            match notify_rust::Notification::new()
                .summary("tsk deadline reached")
                .body(&body)
                .timeout(notify_rust::Timeout::Never)
                .show()
            {
                Ok(_) => {
                    if let Err(e) = app.mark_deadline_notified(task.id) {
                        app.message = Some(format!("could not record deadline notification: {e}"));
                    }
                }
                Err(e) => app.message = Some(format!("desktop notification failed: {e}")),
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
