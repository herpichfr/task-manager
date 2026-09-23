//! The task/note editing form: field cycling, small UTF-8-safe text
//! cursors (Title, Start, Deadline all share the same model), and dropdown
//! handoff for status/priority/tags.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::domain::dates;
use crate::domain::task::{Priority, Status};
use crate::ui::popup::{DropdownState, DropdownTarget, Popup, PopupOutcome, PopupValue, SelectItem};

/// What a form is for. `NewTask` carries the column the new task should
/// land in by default (overridable via the form's own `Status` field); the
/// rest identify an existing row being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormKind {
    NewTask(Status),
    EditTask,
    NewNote,
    EditNote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Status,
    Priority,
    Tags,
    Start,
    Deadline,
    Body,
}

/// Live editing state for one open task or note form.
#[derive(Debug, Clone)]
pub struct FormState {
    pub kind: FormKind,
    pub title: String,
    /// Byte offset into `title`, always on a UTF-8 char boundary.
    pub cursor: usize,
    pub status: Status,
    pub priority: Priority,
    pub tags: Vec<String>,
    /// User-typed date text, parsed with `dates::parse_date` on submit.
    /// Empty means "no date" (parses to `None`, clearing it).
    pub start_text: String,
    pub start_cursor: usize,
    pub deadline_text: String,
    pub deadline_cursor: usize,
    pub body: String,
    pub field: Field,
    pub editing_id: Option<i64>,
    /// Inline validation message (empty title, or an unparseable date),
    /// cleared on the next edit to whichever field raised it.
    pub error: Option<String>,
}

/// The validated result of a submitted form, for `App` to persist.
#[derive(Debug, Clone)]
pub struct TaskDraft {
    pub title: String,
    pub status: Status,
    pub body: String,
    pub priority: Priority,
    pub tags: Vec<String>,
    /// Unix seconds; `None` clears the field.
    pub start_date: Option<i64>,
    /// Unix seconds; `None` clears the field.
    pub deadline: Option<i64>,
    pub editing_id: Option<i64>,
    pub kind: FormKind,
}

const TASK_FIELDS: [Field; 7] =
    [Field::Title, Field::Status, Field::Priority, Field::Tags, Field::Start, Field::Deadline, Field::Body];
const NOTE_FIELDS: [Field; 2] = [Field::Title, Field::Body];

fn status_label(status: Status) -> &'static str {
    match status {
        Status::ToDo => "ToDo",
        Status::Doing => "Doing",
        Status::Done => "Done",
    }
}

// --- free-standing UTF-8-safe text-cursor helpers, shared by every text
// field (Title, Start, Deadline) so the model exists exactly once ---------

fn prev_char_boundary(s: &str, idx: usize) -> usize {
    if idx == 0 {
        return 0;
    }
    let mut i = idx - 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx + 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn text_insert_char(text: &mut String, cursor: &mut usize, c: char) {
    text.insert(*cursor, c);
    *cursor += c.len_utf8();
}

fn text_backspace(text: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let start = prev_char_boundary(text, *cursor);
    text.replace_range(start..*cursor, "");
    *cursor = start;
}

fn text_delete_forward(text: &mut String, cursor: &mut usize) {
    if *cursor >= text.len() {
        return;
    }
    let end = next_char_boundary(text, *cursor);
    text.replace_range(*cursor..end, "");
}

fn text_cursor_left(text: &str, cursor: &mut usize) {
    *cursor = prev_char_boundary(text, *cursor);
}

fn text_cursor_right(text: &str, cursor: &mut usize) {
    *cursor = next_char_boundary(text, *cursor);
}

impl FormState {
    pub fn new_task(status: Status) -> Self {
        FormState {
            kind: FormKind::NewTask(status),
            title: String::new(),
            cursor: 0,
            status,
            priority: Priority::default(),
            tags: Vec::new(),
            start_text: String::new(),
            start_cursor: 0,
            deadline_text: String::new(),
            deadline_cursor: 0,
            body: String::new(),
            field: Field::Title,
            editing_id: None,
            error: None,
        }
    }

    pub fn new_note() -> Self {
        FormState {
            kind: FormKind::NewNote,
            title: String::new(),
            cursor: 0,
            status: Status::ToDo,
            priority: Priority::default(),
            tags: Vec::new(),
            start_text: String::new(),
            start_cursor: 0,
            deadline_text: String::new(),
            deadline_cursor: 0,
            body: String::new(),
            field: Field::Title,
            editing_id: None,
            error: None,
        }
    }

    /// Prefills an edit-task form. `title`/`body`/`priority`/`status`/
    /// `start_date`/`deadline` are the task's current values; the date
    /// fields are prefilled via `dates::format_date`, empty when unset.
    #[allow(clippy::too_many_arguments)]
    pub fn edit_task(
        id: i64,
        title: String,
        body: String,
        priority: Priority,
        status: Status,
        start_date: Option<i64>,
        deadline: Option<i64>,
        tags: Vec<String>,
    ) -> Self {
        let cursor = title.len();
        let start_text = start_date.map(dates::format_date).unwrap_or_default();
        let deadline_text = deadline.map(dates::format_date).unwrap_or_default();
        let start_cursor = start_text.len();
        let deadline_cursor = deadline_text.len();
        FormState {
            kind: FormKind::EditTask,
            title,
            cursor,
            status,
            priority,
            tags,
            start_text,
            start_cursor,
            deadline_text,
            deadline_cursor,
            body,
            field: Field::Title,
            editing_id: Some(id),
            error: None,
        }
    }

    /// Prefills an edit-note form, splitting the note's single `body`
    /// string into a title (first line) and rest (remaining lines) using
    /// the same convention `promote_note` uses.
    pub fn edit_note(id: i64, body: &str) -> Self {
        let mut lines = body.splitn(2, '\n');
        let title = lines.next().unwrap_or("").to_string();
        let rest = lines.next().unwrap_or("").to_string();
        let cursor = title.len();
        FormState {
            kind: FormKind::EditNote,
            title,
            cursor,
            status: Status::ToDo,
            priority: Priority::default(),
            tags: Vec::new(),
            start_text: String::new(),
            start_cursor: 0,
            deadline_text: String::new(),
            deadline_cursor: 0,
            body: rest,
            field: Field::Title,
            editing_id: Some(id),
            error: None,
        }
    }

    pub fn is_note(&self) -> bool {
        matches!(self.kind, FormKind::NewNote | FormKind::EditNote)
    }

    fn fields(&self) -> &'static [Field] {
        if self.is_note() {
            &NOTE_FIELDS
        } else {
            &TASK_FIELDS
        }
    }

    fn field_index(&self) -> usize {
        self.fields().iter().position(|f| *f == self.field).unwrap_or(0)
    }

    fn next_field(&mut self) {
        let fields = self.fields();
        let i = (self.field_index() + 1) % fields.len();
        self.field = fields[i];
    }

    fn prev_field(&mut self) {
        let fields = self.fields();
        let i = self.field_index();
        let i = if i == 0 { fields.len() - 1 } else { i - 1 };
        self.field = fields[i];
    }

    // --- Title cursor model (UTF-8 safe); Start/Deadline share the same
    // free functions directly in `handle_date_key` below ------------------

    pub fn insert_char(&mut self, c: char) {
        text_insert_char(&mut self.title, &mut self.cursor, c);
    }

    pub fn backspace(&mut self) {
        text_backspace(&mut self.title, &mut self.cursor);
    }

    pub fn delete_forward(&mut self) {
        text_delete_forward(&mut self.title, &mut self.cursor);
    }

    pub fn cursor_left(&mut self) {
        text_cursor_left(&self.title, &mut self.cursor);
    }

    pub fn cursor_right(&mut self) {
        text_cursor_right(&self.title, &mut self.cursor);
    }

    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor = self.title.len();
    }

    // --- key handling ----------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) -> PopupOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
            return PopupOutcome::Close;
        }
        if ctrl && key.code == KeyCode::Char('s') {
            return self.try_submit();
        }
        match key.code {
            KeyCode::Tab => {
                self.next_field();
                return PopupOutcome::Consumed;
            }
            KeyCode::BackTab => {
                self.prev_field();
                return PopupOutcome::Consumed;
            }
            _ => {}
        }

        match self.field {
            Field::Title => self.handle_title_key(key),
            Field::Status => self.handle_status_key(key),
            Field::Priority => self.handle_priority_key(key),
            Field::Tags => self.handle_tags_key(key),
            Field::Start => self.handle_date_key(key, true),
            Field::Deadline => self.handle_date_key(key, false),
            Field::Body => PopupOutcome::Consumed, // Enter on Body is handled by App (needs the terminal).
        }
    }

    fn handle_title_key(&mut self, key: KeyEvent) -> PopupOutcome {
        match key.code {
            KeyCode::Char(c) => {
                self.insert_char(c);
                self.error = None;
            }
            KeyCode::Backspace => {
                self.backspace();
                self.error = None;
            }
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Left => self.cursor_left(),
            KeyCode::Right => self.cursor_right(),
            KeyCode::Home => self.cursor_home(),
            KeyCode::End => self.cursor_end(),
            _ => {}
        }
        PopupOutcome::Consumed
    }

    /// Shared by the Start and Deadline fields: identical editing keys,
    /// operating on whichever of the two text/cursor pairs `is_start`
    /// selects.
    fn handle_date_key(&mut self, key: KeyEvent, is_start: bool) -> PopupOutcome {
        {
            let (text, cursor) = if is_start {
                (&mut self.start_text, &mut self.start_cursor)
            } else {
                (&mut self.deadline_text, &mut self.deadline_cursor)
            };
            match key.code {
                KeyCode::Char(c) => text_insert_char(text, cursor, c),
                KeyCode::Backspace => text_backspace(text, cursor),
                KeyCode::Delete => text_delete_forward(text, cursor),
                KeyCode::Left => text_cursor_left(text, cursor),
                KeyCode::Right => text_cursor_right(text, cursor),
                KeyCode::Home => *cursor = 0,
                KeyCode::End => *cursor = text.len(),
                _ => {}
            }
        }
        self.error = None;
        PopupOutcome::Consumed
    }

    fn handle_status_key(&mut self, key: KeyEvent) -> PopupOutcome {
        if key.code == KeyCode::Enter {
            let selected = Status::ALL.iter().position(|s| *s == self.status).unwrap_or(0);
            let items = Status::ALL
                .iter()
                .enumerate()
                .map(|(i, s)| SelectItem { id: i as i64, label: status_label(*s).to_string() })
                .collect();
            return PopupOutcome::Push(Box::new(Popup::Dropdown(DropdownState {
                title: "Status".to_string(),
                items,
                selected,
                target: DropdownTarget::Status,
            })));
        }
        PopupOutcome::Consumed
    }

    fn handle_priority_key(&mut self, key: KeyEvent) -> PopupOutcome {
        if key.code == KeyCode::Enter {
            let selected = Priority::ALL.iter().position(|p| *p == self.priority).unwrap_or(0);
            let items = Priority::ALL
                .iter()
                .map(|p| SelectItem { id: *p as i64, label: p.as_str().to_string() })
                .collect();
            return PopupOutcome::Push(Box::new(Popup::Dropdown(DropdownState {
                title: "Priority".to_string(),
                items,
                selected,
                target: DropdownTarget::Priority,
            })));
        }
        PopupOutcome::Consumed
    }

    /// Enter on this field is intercepted by `App::route_key_to_popup`
    /// before it reaches here, since building the board-wide tag list
    /// needs the store, which `FormState` deliberately has no access to.
    /// Any other key on this field does nothing.
    fn handle_tags_key(&mut self, _key: KeyEvent) -> PopupOutcome {
        PopupOutcome::Consumed
    }

    /// Applies a value received from a child popup (a status/priority/tag
    /// dropdown submitting back into this form). Only meaningful while
    /// `self.field` is `Status`, `Priority`, or `Tags`, since those are the
    /// only fields that push a child popup.
    pub fn receive(&mut self, value: PopupValue) {
        match (self.field, value) {
            (Field::Status, PopupValue::Selected(item)) => {
                if let Some(s) = Status::ALL.get(item.id as usize) {
                    self.status = *s;
                }
            }
            (Field::Priority, PopupValue::Selected(item)) => {
                if let Some(p) = Priority::ALL.iter().find(|p| **p as i64 == item.id) {
                    self.priority = *p;
                }
            }
            // The "+ new tag…" row of the form-scoped tag picker
            // (`App::open_form_tag_picker`) replaces itself with a
            // one-line `TextPrompt`; this form is still underneath it on
            // the popup stack, so the typed name arrives here rather than
            // through `App::apply_text`. Persisted for real -- upserted by
            // name and attached via `set_task_tags` -- only when the form
            // itself is submitted (`try_submit`).
            (Field::Tags, PopupValue::Text(name)) => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    self.error = Some("tag name must not be empty".to_string());
                } else {
                    self.error = None;
                    if !self.tags.contains(&name) {
                        self.tags.push(name);
                    }
                }
            }
            _ => {}
        }
    }

    fn try_submit(&mut self) -> PopupOutcome {
        if self.title.trim().is_empty() {
            self.error = Some("title cannot be empty".to_string());
            return PopupOutcome::Consumed;
        }
        let now = chrono::Utc::now().timestamp();
        let start_date = match dates::parse_date(&self.start_text, now) {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(format!("start: {e}"));
                return PopupOutcome::Consumed;
            }
        };
        let deadline = match dates::parse_date(&self.deadline_text, now) {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(format!("deadline: {e}"));
                return PopupOutcome::Consumed;
            }
        };
        PopupOutcome::Submit(PopupValue::Form(TaskDraft {
            title: self.title.clone(),
            status: self.status,
            body: self.body.clone(),
            priority: self.priority,
            tags: self.tags.clone(),
            start_date,
            deadline,
            editing_id: self.editing_id,
            kind: self.kind,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn insert_appends_and_advances_cursor() {
        let mut f = FormState::new_task(Status::ToDo);
        f.insert_char('h');
        f.insert_char('i');
        assert_eq!(f.title, "hi");
        assert_eq!(f.cursor, 2);
    }

    #[test]
    fn backspace_removes_previous_char_and_moves_cursor() {
        let mut f = FormState::new_task(Status::ToDo);
        f.title = "hi".to_string();
        f.cursor = 2;
        f.backspace();
        assert_eq!(f.title, "h");
        assert_eq!(f.cursor, 1);
    }

    #[test]
    fn left_right_home_end_move_cursor() {
        let mut f = FormState::new_task(Status::ToDo);
        f.title = "abc".to_string();
        f.cursor = 3;
        f.cursor_left();
        assert_eq!(f.cursor, 2);
        f.cursor_right();
        assert_eq!(f.cursor, 3);
        f.cursor_home();
        assert_eq!(f.cursor, 0);
        f.cursor_end();
        assert_eq!(f.cursor, 3);
    }

    #[test]
    fn utf8_insert_into_multibyte_string_stays_on_char_boundaries() {
        let mut f = FormState::new_task(Status::ToDo);
        f.title = "héllo".to_string();
        // 'é' is 2 bytes; cursor after 'h' + 'é' is byte offset 3.
        f.cursor = 3;
        f.insert_char('!');
        assert_eq!(f.title, "hé!llo");
    }

    #[test]
    fn utf8_backspace_removes_whole_multibyte_char() {
        let mut f = FormState::new_task(Status::ToDo);
        f.title = "héllo".to_string();
        f.cursor = 3; // right after the é
        f.backspace();
        assert_eq!(f.title, "hllo");
        assert_eq!(f.cursor, 1);
    }

    #[test]
    fn utf8_delete_forward_removes_whole_multibyte_char() {
        let mut f = FormState::new_task(Status::ToDo);
        f.title = "héllo".to_string();
        f.cursor = 1; // right before the é
        f.delete_forward();
        assert_eq!(f.title, "hllo");
        assert_eq!(f.cursor, 1);
    }

    #[test]
    fn tab_and_backtab_cycle_all_task_fields() {
        let mut f = FormState::new_task(Status::ToDo);
        let order = [
            Field::Title,
            Field::Status,
            Field::Priority,
            Field::Tags,
            Field::Start,
            Field::Deadline,
            Field::Body,
        ];
        assert_eq!(f.field, order[0]);
        for expected in order.iter().skip(1) {
            f.handle_key(key(KeyCode::Tab));
            assert_eq!(f.field, *expected);
        }
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.field, Field::Title);
        f.handle_key(key(KeyCode::BackTab));
        assert_eq!(f.field, Field::Body);
    }

    #[test]
    fn note_form_only_cycles_title_and_body() {
        let mut f = FormState::new_note();
        assert_eq!(f.field, Field::Title);
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.field, Field::Body);
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.field, Field::Title);
    }

    #[test]
    fn empty_title_rejected_on_submit() {
        let mut f = FormState::new_task(Status::ToDo);
        let outcome = f.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(outcome, PopupOutcome::Consumed));
        assert_eq!(f.error.as_deref(), Some("title cannot be empty"));
    }

    #[test]
    fn non_empty_title_submits_a_draft() {
        let mut f = FormState::new_task(Status::ToDo);
        f.title = "write the plan".to_string();
        let outcome = f.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        match outcome {
            PopupOutcome::Submit(PopupValue::Form(draft)) => {
                assert_eq!(draft.title, "write the plan");
                assert_eq!(draft.kind, FormKind::NewTask(Status::ToDo));
                assert_eq!(draft.status, Status::ToDo);
                assert_eq!(draft.start_date, None);
                assert_eq!(draft.deadline, None);
            }
            other => panic!("expected Submit(Form(..)), got {other:?}"),
        }
    }

    #[test]
    fn enter_on_priority_field_pushes_a_dropdown() {
        let mut f = FormState::new_task(Status::ToDo);
        f.field = Field::Priority;
        let outcome = f.handle_key(key(KeyCode::Enter));
        match outcome {
            PopupOutcome::Push(boxed) => match *boxed {
                Popup::Dropdown(d) => {
                    assert_eq!(d.target, DropdownTarget::Priority);
                    assert_eq!(d.items.len(), Priority::ALL.len());
                }
                _ => panic!("expected Dropdown"),
            },
            other => panic!("expected Push(Dropdown), got {other:?}"),
        }
    }

    #[test]
    fn receive_selected_priority_updates_form() {
        let mut f = FormState::new_task(Status::ToDo);
        f.field = Field::Priority;
        f.receive(PopupValue::Selected(SelectItem { id: Priority::Urgent as i64, label: "urgent".to_string() }));
        assert_eq!(f.priority, Priority::Urgent);
    }

    #[test]
    fn edit_note_splits_title_and_body() {
        let f = FormState::edit_note(1, "Buy groceries\nmilk, eggs");
        assert_eq!(f.title, "Buy groceries");
        assert_eq!(f.body, "milk, eggs");
    }

    // --- Status field ------------------------------------------------------

    #[test]
    fn enter_on_status_field_pushes_a_dropdown_with_three_items() {
        let mut f = FormState::new_task(Status::ToDo);
        f.field = Field::Status;
        let outcome = f.handle_key(key(KeyCode::Enter));
        match outcome {
            PopupOutcome::Push(boxed) => match *boxed {
                Popup::Dropdown(d) => {
                    assert_eq!(d.target, DropdownTarget::Status);
                    assert_eq!(d.items.len(), 3);
                }
                _ => panic!("expected Dropdown"),
            },
            other => panic!("expected Push(Dropdown), got {other:?}"),
        }
    }

    #[test]
    fn receive_selected_status_updates_form() {
        let mut f = FormState::new_task(Status::ToDo);
        f.field = Field::Status;
        let idx = Status::ALL.iter().position(|s| *s == Status::Done).unwrap();
        f.receive(PopupValue::Selected(SelectItem { id: idx as i64, label: "Done".to_string() }));
        assert_eq!(f.status, Status::Done);
    }

    // --- Start/Deadline date fields -----------------------------------------

    #[test]
    fn typing_into_deadline_field_and_submitting_parses_it() {
        let mut f = FormState::new_task(Status::ToDo);
        f.title = "ship it".to_string();
        f.field = Field::Deadline;
        for c in "2030-01-15".chars() {
            f.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(f.deadline_text, "2030-01-15");
        let outcome = f.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        match outcome {
            PopupOutcome::Submit(PopupValue::Form(draft)) => {
                assert_eq!(dates::format_date(draft.deadline.unwrap()), "2030-01-15");
            }
            other => panic!("expected Submit(Form(..)), got {other:?}"),
        }
    }

    #[test]
    fn unparseable_deadline_blocks_submit_with_inline_error() {
        let mut f = FormState::new_task(Status::ToDo);
        f.title = "ship it".to_string();
        f.deadline_text = "not a date".to_string();
        let outcome = f.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(outcome, PopupOutcome::Consumed));
        assert!(f.error.is_some());
    }

    #[test]
    fn unparseable_start_date_blocks_submit_with_inline_error() {
        let mut f = FormState::new_task(Status::ToDo);
        f.title = "ship it".to_string();
        f.start_text = "garbage".to_string();
        let outcome = f.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(outcome, PopupOutcome::Consumed));
        assert!(f.error.is_some());
    }

    #[test]
    fn clearing_date_text_clears_the_date_on_submit() {
        let mut f = FormState::edit_task(1, "t".to_string(), String::new(), Priority::Normal, Status::ToDo, Some(1_700_000_000), None, Vec::new());
        assert_eq!(f.start_text, dates::format_date(1_700_000_000));
        f.start_text.clear();
        let outcome = f.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        match outcome {
            PopupOutcome::Submit(PopupValue::Form(draft)) => assert_eq!(draft.start_date, None),
            other => panic!("expected Submit(Form(..)), got {other:?}"),
        }
    }

    #[test]
    fn edit_task_prefills_dates_and_status() {
        let f = FormState::edit_task(
            5,
            "title".to_string(),
            "body".to_string(),
            Priority::High,
            Status::Doing,
            Some(1_700_000_000),
            Some(1_800_000_000),
            Vec::new(),
        );
        assert_eq!(f.status, Status::Doing);
        assert_eq!(f.start_text, dates::format_date(1_700_000_000));
        assert_eq!(f.deadline_text, dates::format_date(1_800_000_000));
    }

    #[test]
    fn edit_task_prefills_tags() {
        let f = FormState::edit_task(
            5,
            "title".to_string(),
            "body".to_string(),
            Priority::High,
            Status::Doing,
            None,
            None,
            vec!["home".to_string(), "urgent".to_string()],
        );
        assert_eq!(f.tags, vec!["home".to_string(), "urgent".to_string()]);
    }

    #[test]
    fn receive_text_on_tags_field_appends_a_new_tag() {
        let mut f = FormState::new_task(Status::ToDo);
        f.field = Field::Tags;
        f.receive(PopupValue::Text("urgent".to_string()));
        assert_eq!(f.tags, vec!["urgent".to_string()]);
    }

    #[test]
    fn receive_text_on_tags_field_ignores_a_duplicate() {
        let mut f = FormState::new_task(Status::ToDo);
        f.field = Field::Tags;
        f.tags.push("urgent".to_string());
        f.receive(PopupValue::Text("urgent".to_string()));
        assert_eq!(f.tags, vec!["urgent".to_string()]);
    }

    #[test]
    fn receive_empty_text_on_tags_field_sets_an_inline_error() {
        let mut f = FormState::new_task(Status::ToDo);
        f.field = Field::Tags;
        f.receive(PopupValue::Text("   ".to_string()));
        assert!(f.tags.is_empty());
        assert_eq!(f.error.as_deref(), Some("tag name must not be empty"));
    }

    #[test]
    fn receive_text_clears_stale_error() {
        let mut f = FormState::new_task(Status::ToDo);
        f.field = Field::Tags;
        f.receive(PopupValue::Text("   ".to_string()));
        assert_eq!(f.error.as_deref(), Some("tag name must not be empty"));
        f.receive(PopupValue::Text("home".to_string()));
        assert_eq!(f.error, None);
        assert_eq!(f.tags, vec!["home".to_string()]);
    }
}
