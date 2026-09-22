//! Renders the three-column kanban board (and, when toggled on, the notes
//! pane below it).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::domain::board::BoardKind;
use crate::domain::task::{Priority, Status};
use crate::ui::theme::Styles;

pub fn render(frame: &mut Frame, area: Rect, app: &App, styles: &Styles) {
    let outer = Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).split(area);
    let title_area = outer[0];
    let body_area = outer[1];

    let lock_marker = if app.board.kind == BoardKind::Locked { " \u{1F512}" } else { "" };
    let title_line = Line::from(Span::styled(
        format!(" {}{} ", app.board.name, lock_marker),
        styles.default.add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(title_line), title_area);

    let (board_area, notes_area) = if app.show_notes {
        let chunks = Layout::vertical([Constraint::Percentage(70), Constraint::Percentage(30)]).split(body_area);
        (chunks[0], Some(chunks[1]))
    } else {
        (body_area, None)
    };

    let cols = Layout::horizontal([Constraint::Ratio(1, 3); 3]).split(board_area);

    for (idx, status) in Status::ALL.iter().enumerate() {
        render_column(frame, cols[idx], app, styles, *status, idx);
    }

    if let Some(notes_rect) = notes_area {
        render_notes(frame, notes_rect, app, styles);
    }
}

fn status_label_and_style(status: Status, styles: &Styles) -> (&'static str, ratatui::style::Style) {
    match status {
        Status::ToDo => ("ToDo", styles.todo),
        Status::Doing => ("Doing", styles.doing),
        Status::Done => ("Done", styles.done),
    }
}

fn priority_style(priority: Priority, styles: &Styles) -> ratatui::style::Style {
    match priority {
        Priority::Low => styles.priority_low,
        Priority::Normal => styles.priority_normal,
        Priority::High => styles.priority_high,
        Priority::Urgent => styles.priority_urgent,
    }
}

fn priority_marker(priority: Priority) -> &'static str {
    match priority {
        Priority::Urgent => "!! ",
        Priority::High => "! ",
        Priority::Normal | Priority::Low => "",
    }
}

fn render_column(frame: &mut Frame, area: Rect, app: &App, styles: &Styles, status: Status, idx: usize) {
    let col = &app.columns[idx];
    let (label, status_style) = status_label_and_style(status, styles);
    let title = format!(" {} ({}) ", label, col.tasks.len());
    let focused = idx == app.focused;

    let block = Block::bordered().title(Line::from(title)).border_style(status_style);
    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = if col.tasks.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "—",
            styles.default.add_modifier(Modifier::DIM),
        )))]
    } else {
        col.tasks
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let marker = priority_marker(task.priority);
                let budget = inner_width.saturating_sub(marker.chars().count());
                let title = truncate_with_ellipsis(&task.title, budget);
                let text = format!("{marker}{title}");

                let base_style = priority_style(task.priority, styles);
                let style = if i == col.selected {
                    if focused {
                        styles.selection
                    } else {
                        base_style.add_modifier(Modifier::DIM)
                    }
                } else {
                    base_style
                };
                ListItem::new(Line::from(Span::styled(text, style)))
            })
            .collect()
    };

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn render_notes(frame: &mut Frame, area: Rect, app: &App, styles: &Styles) {
    let block = Block::bordered()
        .title(Line::from(" Notes "))
        .border_style(styles.border);
    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = if app.notes.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "—",
            styles.default.add_modifier(Modifier::DIM),
        )))]
    } else {
        app.notes
            .iter()
            .map(|note| {
                let first_line = note.body.lines().next().unwrap_or("");
                let text = truncate_with_ellipsis(first_line, inner_width);
                ListItem::new(Line::from(Span::styled(text, styles.default)))
            })
            .collect()
    };

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

/// Truncates `s` to at most `max` display characters, replacing anything
/// cut off with a single trailing ellipsis. Never wraps, never splits a
/// unicode scalar mid-character.
fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max == 1 {
        return "…".to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Phase 5: end-to-end keyboard-driven task/note editing -----------
    //
    // These drive `App` through real key sequences against an in-memory
    // DB, exercising the popup stack, undo/redo, and storage together.
    // Self-contained helpers (rather than reusing `ui::tests`'s, which are
    // private to that module).

    use crate::config::Config;
    use crate::domain::board::BoardKind;
    use crate::domain::task::NewTask;
    use crate::storage::main_db::MainDb;
    use crate::storage::TaskStore;
    use crate::ui::popup::Popup;
    use crossterm::event::{KeyCode as K, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend as Phase5TestBackend;
    use ratatui::Terminal as Phase5Terminal;

    fn phase5_test_app(todo: usize, doing: usize, done: usize) -> crate::app::App {
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
                        })
                        .unwrap();
                }
            }
        }
        let board = db.get_board_by_name("test").unwrap().unwrap();
        crate::app::App::new(Config::default(), db, board).unwrap()
    }

    fn phase5_buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
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

    fn press_char(app: &mut crate::app::App, c: char) {
        app.handle_key(KeyEvent::new(K::Char(c), KeyModifiers::NONE));
    }

    fn press_ctrl(app: &mut crate::app::App, c: char) {
        app.handle_key(KeyEvent::new(K::Char(c), KeyModifiers::CONTROL));
    }

    #[test]
    fn a_type_title_ctrl_s_creates_task_with_right_status_and_position() {
        let mut app = phase5_test_app(0, 0, 0);
        press_char(&mut app, 'a');
        assert!(matches!(app.popups.last(), Some(Popup::Form(_))));
        for c in "buy milk".chars() {
            press_char(&mut app, c);
        }
        press_ctrl(&mut app, 's');
        assert!(app.popups.is_empty());
        assert_eq!(app.columns[0].tasks.len(), 1);
        assert_eq!(app.columns[0].tasks[0].title, "buy milk");
        assert_eq!(app.columns[0].tasks[0].status, Status::ToDo);
        assert_eq!(app.columns[0].tasks[0].position, 0);
    }

    #[test]
    fn shift_l_moves_task_to_doing_and_keeps_both_columns_dense() {
        let mut app = phase5_test_app(2, 1, 0);
        app.handle_key(KeyEvent::new(K::Char('L'), KeyModifiers::SHIFT));
        assert_eq!(app.columns[0].tasks.len(), 1);
        assert_eq!(app.columns[1].tasks.len(), 2);
        let positions: Vec<i64> = app.columns[0].tasks.iter().map(|t| t.position).collect();
        assert_eq!(positions, vec![0]);
        let positions: Vec<i64> = app.columns[1].tasks.iter().map(|t| t.position).collect();
        assert_eq!(positions, vec![0, 1]);
    }

    #[test]
    fn dd_confirm_deletes_then_u_restores() {
        let mut app = phase5_test_app(1, 0, 0);
        let title = app.columns[0].tasks[0].title.clone();
        press_char(&mut app, 'd');
        press_char(&mut app, 'd');
        assert!(matches!(app.popups.last(), Some(Popup::Confirm(_))));
        press_char(&mut app, 'y');
        assert!(app.popups.is_empty());
        assert_eq!(app.columns[0].tasks.len(), 0);

        press_char(&mut app, 'u');
        assert_eq!(app.columns[0].tasks.len(), 1);
        assert_eq!(app.columns[0].tasks[0].title, title);
    }

    #[test]
    fn p_pick_urgent_updates_priority() {
        let mut app = phase5_test_app(1, 0, 0);
        press_char(&mut app, 'p');
        assert!(matches!(app.popups.last(), Some(Popup::Dropdown(_))));
        if let Some(Popup::Dropdown(d)) = app.popups.last_mut() {
            let idx = d.items.iter().position(|i| i.label == "urgent").unwrap();
            d.selected = idx;
        }
        app.handle_key(KeyEvent::new(K::Enter, KeyModifiers::NONE));
        assert!(app.popups.is_empty());
        assert_eq!(app.columns[0].tasks[0].priority, Priority::Urgent);
    }

    #[test]
    fn empty_title_is_rejected_end_to_end() {
        let mut app = phase5_test_app(0, 0, 0);
        press_char(&mut app, 'a');
        press_ctrl(&mut app, 's');
        assert!(!app.popups.is_empty(), "form should stay open on empty title");
        assert_eq!(app.columns[0].tasks.len(), 0);
    }

    #[test]
    fn form_popup_renders_over_the_board() {
        let mut app = phase5_test_app(1, 0, 0);
        press_char(&mut app, 'a');
        for c in "write the plan".chars() {
            press_char(&mut app, c);
        }
        let backend = Phase5TestBackend::new(80, 24);
        let mut terminal = Phase5Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::render(f, &app)).unwrap();
        let rendered = phase5_buffer_to_string(terminal.backend().buffer());
        println!("--- task form over board ---\n{rendered}");
        assert!(rendered.contains("New task"));
        assert!(rendered.contains("write the plan"));
    }

    #[test]
    fn priority_dropdown_renders_over_the_form() {
        let mut app = phase5_test_app(1, 0, 0);
        press_char(&mut app, 'a');
        for c in "write the plan".chars() {
            press_char(&mut app, c);
        }
        app.handle_key(KeyEvent::new(K::Tab, KeyModifiers::NONE)); // Title -> Priority
        app.handle_key(KeyEvent::new(K::Enter, KeyModifiers::NONE)); // open Priority dropdown
        let backend = Phase5TestBackend::new(80, 24);
        let mut terminal = Phase5Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::render(f, &app)).unwrap();
        let rendered = phase5_buffer_to_string(terminal.backend().buffer());
        println!("--- priority dropdown over task form ---\n{rendered}");
        assert!(rendered.contains("Priority"));
        assert!(rendered.contains("New task"));
        assert!(rendered.contains("urgent"));
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate_with_ellipsis("hi", 10), "hi");
    }

    #[test]
    fn truncate_long_string_gets_ellipsis() {
        let out = truncate_with_ellipsis("hello world", 5);
        assert_eq!(out, "hell…");
        assert_eq!(out.chars().count(), 5);
    }

    #[test]
    fn truncate_zero_width_is_empty() {
        assert_eq!(truncate_with_ellipsis("hello", 0), "");
    }

    #[test]
    fn truncate_width_one_is_just_ellipsis() {
        assert_eq!(truncate_with_ellipsis("hello", 1), "…");
    }

    #[test]
    fn locked_board_title_shows_lock_marker() {
        let mut app = phase5_test_app(0, 0, 0);
        app.board.kind = BoardKind::Locked;
        let backend = Phase5TestBackend::new(80, 24);
        let mut terminal = Phase5Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::render(f, &app)).unwrap();
        let rendered = phase5_buffer_to_string(terminal.backend().buffer());
        assert!(rendered.contains('\u{1F512}'), "rendered:\n{rendered}");
    }
}
