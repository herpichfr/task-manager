//! The TUI's rendering entry point and its constituent widgets.

pub mod board;
pub mod footer;
pub mod forms;
pub mod popup;
pub mod theme;

use ratatui::layout::{Constraint, Layout};
use ratatui::Frame;

use crate::app::App;

/// Renders one full frame: the board (with its optional notes pane) filling
/// everything but the last two rows, the footer hint/status bar in those
/// last two rows, and -- on top of both -- the popup stack (a dropdown, a
/// task/note form, a confirmation prompt, a passphrase prompt, or the help
/// screen), if any is open.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(area);

    board::render(frame, chunks[0], app, &app.styles);
    footer::render(frame, chunks[1], app, &app.styles);
    popup::render(frame, area, &app.popups, &app.styles);
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    use crate::app::App;
    use crate::config::Config;
    use crate::domain::board::BoardKind;
    use crate::domain::task::{NewTask, Priority, Status};
    use crate::storage::main_db::MainDb;
    use crate::storage::TaskStore;

    fn test_app(todo: usize, doing: usize, done: usize) -> App {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("test", BoardKind::Plain).unwrap();
        {
            let store = db.store_for(board_id);
            for (n, status) in [(todo, Status::ToDo), (doing, Status::Doing), (done, Status::Done)] {
                for i in 0..n {
                    store
                        .create_task(NewTask {
                            title: format!("{status:?} task {i}"),
                            body: String::new(),
                            status,
                            priority: Priority::Normal,
                            start_date: None,
                            time_expected: None, deadline: None,
                        })
                        .unwrap();
                }
            }
        }
        let board = db.get_board_by_name("test").unwrap().unwrap();
        App::new(Config::default(), db, board).unwrap()
    }

    fn buffer_to_string(buf: &Buffer) -> String {
        let area = buf.area;
        let mut s = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    /// Drives `app` through `keys` (each an ordinary Normal-mode key press,
    /// no modifiers) at an 80x24 viewport, then renders one frame.
    pub(crate) fn drive(app: &mut App, keys: &[KeyCode]) -> Buffer {
        drive_sized(app, keys, 80, 24)
    }

    pub(crate) fn drive_sized(app: &mut App, keys: &[KeyCode], width: u16, height: u16) -> Buffer {
        for &code in keys {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| super::render(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn board_renders_three_columns_with_counts() {
        let mut app = test_app(2, 1, 3);
        let buf = drive(&mut app, &[]);
        let rendered = buffer_to_string(&buf);
        println!("{rendered}");
        assert!(rendered.contains("ToDo (2)"), "rendered:\n{rendered}");
        assert!(rendered.contains("Doing (1)"), "rendered:\n{rendered}");
        assert!(rendered.contains("Done (3)"), "rendered:\n{rendered}");
    }

    #[test]
    fn jjk_leaves_selection_where_expected() {
        let mut app = test_app(5, 0, 0);
        drive(&mut app, &[KeyCode::Char('j'), KeyCode::Char('j'), KeyCode::Char('k')]);
        assert_eq!(app.columns[0].selected, 1);
    }

    #[test]
    fn gg_and_shift_g_jump_to_ends() {
        let mut app = test_app(5, 0, 0);
        drive(&mut app, &[KeyCode::Char('G')]);
        assert_eq!(app.columns[0].selected, 4);
        drive(&mut app, &[KeyCode::Char('g'), KeyCode::Char('g')]);
        assert_eq!(app.columns[0].selected, 0);
    }

    #[test]
    fn l_moves_focus_to_doing_column() {
        let mut app = test_app(1, 1, 1);
        assert_eq!(app.focused, 0);
        drive(&mut app, &[KeyCode::Char('l')]);
        assert_eq!(app.focused, 1);
    }

    #[test]
    fn footer_hint_bar_present_and_changes_with_notes_pane() {
        let mut app = test_app(1, 0, 0);
        let buf = drive(&mut app, &[]);
        let before = buffer_to_string(&buf);
        assert!(before.contains("NORMAL"), "rendered:\n{before}");

        let buf2 = drive(&mut app, &[KeyCode::Char('N')]);
        let after = buffer_to_string(&buf2);
        assert!(app.show_notes);
        assert_ne!(before, after, "notes pane toggle should change the rendered frame");
        assert!(after.contains("Notes"), "rendered:\n{after}");
    }

    #[test]
    fn renders_at_small_and_large_sizes_without_panicking() {
        let mut app = test_app(3, 3, 3);
        let _ = drive_sized(&mut app, &[], 60, 20);
        let _ = drive_sized(&mut app, &[], 200, 50);
    }

    #[test]
    fn passphrase_popup_masks_input_never_rendering_typed_characters() {
        let mut app = test_app(1, 0, 0);
        app.popups.push(crate::ui::popup::Popup::Passphrase(crate::ui::popup::PassphraseState {
            prompt: "Passphrase for \"vault\"".to_string(),
            input: "supersecret".to_string(),
            error: None,
        }));
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| super::render(f, &app)).unwrap();
        let rendered = buffer_to_string(terminal.backend().buffer());
        assert!(!rendered.contains("supersecret"), "rendered:\n{rendered}");
        assert!(rendered.contains(&"•".repeat(11)), "rendered:\n{rendered}");
    }
}
