//! Renders the three-column kanban board (and, when toggled on, the notes
//! pane below it).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::{note_matches_query, task_matches_query, App};
use crate::domain::board::BoardKind;
use crate::domain::dates;
use crate::domain::task::{Priority, Status, Task};
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

    let now = chrono::Local::now().timestamp();
    let query = app.search_query();
    for (idx, status) in Status::ALL.iter().enumerate() {
        render_column(frame, cols[idx], app, styles, *status, idx, now, query.as_deref());
    }

    if let Some(notes_rect) = notes_area {
        render_notes(frame, notes_rect, app, styles, query.as_deref());
    }
}

fn status_label_and_style(status: Status, styles: &Styles) -> (&'static str, ratatui::style::Style) {
    match status {
        Status::ToDo => ("ToDo", styles.todo),
        Status::Doing => ("Doing", styles.doing),
        Status::Done => ("Done", styles.done),
    }
}

fn priority_marker(priority: Priority) -> &'static str {
    match priority {
        Priority::Urgent => "!! ",
        Priority::High => "! ",
        Priority::Normal | Priority::Low => "",
    }
}

/// Builds one card's rendered line: `{priority marker}{title [+tags]}`,
/// left-aligned and truncated with an ellipsis exactly as before this
/// phase, plus -- new in this phase -- a right-aligned compact deadline
/// badge (`3d`, `12d`, `-2d` once overdue) fit into the remaining width.
/// A task with no deadline gets no badge and behaves exactly as before.
fn card_line_text(task: &Task, inner_width: usize, now: i64) -> String {
    let marker = priority_marker(task.priority);
    let badge = task.deadline.map(|d| format!("{}d", dates::days_until(d, now)));
    let badge_w = badge.as_deref().map(|s| s.chars().count()).unwrap_or(0);
    let reserve = if badge_w > 0 { badge_w + 1 } else { 0 };
    let left_budget = inner_width.saturating_sub(marker.chars().count()).saturating_sub(reserve);

    let mut left_content = task.title.clone();
    if !task.tags.is_empty() {
        let tag_str = task.tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(",");
        left_content.push_str(" [");
        left_content.push_str(&tag_str);
        left_content.push(']');
    }
    let left_trunc = truncate_with_ellipsis(&left_content, left_budget);

    let used = marker.chars().count() + left_trunc.chars().count();
    let mut line = format!("{marker}{left_trunc}");
    if let Some(badge) = badge {
        let pad = inner_width.saturating_sub(used).saturating_sub(badge_w);
        line.push_str(&" ".repeat(pad));
        line.push_str(&badge);
    }
    line
}

#[allow(clippy::too_many_arguments)]
fn render_column(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    styles: &Styles,
    status: Status,
    idx: usize,
    now: i64,
    query: Option<&str>,
) {
    let col = &app.columns[idx];
    let (label, status_style) = status_label_and_style(status, styles);
    let focused = idx == app.focused;

    let visible: Vec<(usize, &Task)> = col
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| query.map(|q| task_matches_query(t, q)).unwrap_or(true))
        .collect();

    let title = format!(" {} ({}) ", label, visible.len());
    let block = Block::bordered().title(Line::from(title)).border_style(status_style);
    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = if visible.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "—",
            styles.default.add_modifier(Modifier::DIM),
        )))]
    } else {
        visible
            .iter()
            .map(|(i, task)| {
                let urgency = dates::urgency(task.deadline, now);
                let base_style = styles.for_urgency(urgency);
                let text = card_line_text(task, inner_width, now);

                let style = if *i == col.selected {
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

fn render_notes(frame: &mut Frame, area: Rect, app: &App, styles: &Styles, query: Option<&str>) {
    let block = Block::bordered()
        .title(Line::from(" Notes "))
        .border_style(styles.border);
    let inner_width = area.width.saturating_sub(2) as usize;

    let visible: Vec<_> = app
        .notes
        .iter()
        .filter(|n| query.map(|q| note_matches_query(n, q)).unwrap_or(true))
        .collect();

    let items: Vec<ListItem> = if visible.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "—",
            styles.default.add_modifier(Modifier::DIM),
        )))]
    } else {
        visible
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
                            start_date: None,
                            deadline: None,
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
        app.handle_key(KeyEvent::new(K::Tab, KeyModifiers::NONE)); // Title -> Status
        app.handle_key(KeyEvent::new(K::Tab, KeyModifiers::NONE)); // Status -> Priority
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

    // --- card colour by deadline / deadline badge / tags on the card ------

    #[test]
    fn card_line_text_shows_right_aligned_deadline_badge() {
        let now = 0i64;
        let task = Task {
            id: 1,
            board_id: None,
            title: "buy milk".to_string(),
            body: String::new(),
            status: Status::ToDo,
            priority: Priority::Normal,
            position: 0,
            created_at: 0,
            updated_at: 0,
            start_date: None,
            deadline: Some(3 * 86_400),
            tags: Vec::new(),
        };
        let line = card_line_text(&task, 30, now);
        assert!(line.starts_with("buy milk"), "{line:?}");
        assert!(line.ends_with("3d"), "{line:?}");
        assert!(line.chars().count() <= 30);
    }

    #[test]
    fn card_line_text_overdue_badge_is_negative() {
        let now = 0i64;
        let task = Task {
            id: 1,
            board_id: None,
            title: "late".to_string(),
            body: String::new(),
            status: Status::ToDo,
            priority: Priority::Normal,
            position: 0,
            created_at: 0,
            updated_at: 0,
            start_date: None,
            deadline: Some(-2 * 86_400),
            tags: Vec::new(),
        };
        let line = card_line_text(&task, 30, now);
        assert!(line.ends_with("-2d"), "{line:?}");
    }

    #[test]
    fn card_line_text_with_no_deadline_has_no_badge() {
        let task = Task {
            id: 1,
            board_id: None,
            title: "no due date".to_string(),
            body: String::new(),
            status: Status::ToDo,
            priority: Priority::Normal,
            position: 0,
            created_at: 0,
            updated_at: 0,
            start_date: None,
            deadline: None,
            tags: Vec::new(),
        };
        let line = card_line_text(&task, 30, 0);
        assert_eq!(line, "no due date");
    }

    #[test]
    fn card_line_text_includes_tags() {
        use crate::domain::task::Tag;
        let task = Task {
            id: 1,
            board_id: None,
            title: "call bank".to_string(),
            body: String::new(),
            status: Status::ToDo,
            priority: Priority::Normal,
            position: 0,
            created_at: 0,
            updated_at: 0,
            start_date: None,
            deadline: None,
            tags: vec![Tag { id: 1, name: "home".to_string(), color: None }],
        };
        let line = card_line_text(&task, 40, 0);
        assert!(line.contains("[home]"), "{line:?}");
    }

    /// The ASCII render acceptance check: a board with a card in every
    /// urgency bucket, `--nocapture`d so the layout can be reviewed
    /// without a real terminal.
    #[test]
    fn ascii_render_shows_every_urgency_state() {
        let mut app = phase5_test_app(0, 0, 0);
        let now = chrono::Local::now().timestamp();
        let day = 86_400;
        {
            let store = app.db.store_for(app.board.id);
            let mk = |title: &str, deadline: Option<i64>| NewTask {
                title: title.to_string(),
                body: String::new(),
                status: Status::ToDo,
                priority: Priority::Normal,
                start_date: None,
                deadline,
            };
            store.create_task(mk("no due date", None)).unwrap();
            store.create_task(mk("distant task", Some(now + 20 * day))).unwrap();
            store.create_task(mk("soon task", Some(now + 7 * day))).unwrap();
            store.create_task(mk("near task", Some(now + 3 * day))).unwrap();
            store.create_task(mk("imminent task", Some(now + day))).unwrap();
            store.create_task(mk("overdue task", Some(now - 2 * day))).unwrap();
        }
        app.reload().unwrap();

        let backend = Phase5TestBackend::new(100, 30);
        let mut terminal = Phase5Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::render(f, &app)).unwrap();
        let rendered = phase5_buffer_to_string(terminal.backend().buffer());
        println!("--- board: every urgency state ---\n{rendered}");
        assert!(rendered.contains("no due date"));
        assert!(rendered.contains("distant task"));
        assert!(rendered.contains("soon task"));
        assert!(rendered.contains("near task"));
        assert!(rendered.contains("imminent task"));
        assert!(rendered.contains("overdue task"));
    }

    // --- search filtering ---------------------------------------------------

    #[test]
    fn search_hides_non_matching_cards_and_updates_column_count() {
        let mut app = phase5_test_app(0, 0, 0);
        {
            let store = app.db.store_for(app.board.id);
            for title in ["buy milk", "buy eggs", "call bank"] {
                store
                    .create_task(NewTask {
                        title: title.to_string(),
                        body: String::new(),
                        status: Status::ToDo,
                        priority: Priority::Normal,
                        start_date: None,
                        deadline: None,
                    })
                    .unwrap();
            }
        }
        app.reload().unwrap();
        press_char(&mut app, '/');
        for c in "bank".chars() {
            press_char(&mut app, c);
        }
        let backend = Phase5TestBackend::new(80, 24);
        let mut terminal = Phase5Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::render(f, &app)).unwrap();
        let rendered = phase5_buffer_to_string(terminal.backend().buffer());
        assert!(rendered.contains("call bank"), "rendered:\n{rendered}");
        assert!(!rendered.contains("buy milk"), "rendered:\n{rendered}");
        assert!(rendered.contains("ToDo (1)"), "rendered:\n{rendered}");
    }
}
