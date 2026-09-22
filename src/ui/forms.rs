//! The task/note editing form: field cycling, a small UTF-8-safe text
//! cursor for the title, and dropdown handoff for priority/tags.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::domain::task::{Priority, Status};
use crate::ui::popup::{DropdownState, DropdownTarget, Popup, PopupOutcome, PopupValue, SelectItem};

/// What a form is for. `NewTask` carries the column the new task should
/// land in; the rest identify an existing row being edited.
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
    Priority,
    Tags,
    Body,
}

/// Live editing state for one open task or note form.
#[derive(Debug, Clone)]
pub struct FormState {
    pub kind: FormKind,
    pub title: String,
    /// Byte offset into `title`, always on a UTF-8 char boundary.
    pub cursor: usize,
    pub priority: Priority,
    pub tags: Vec<String>,
    pub body: String,
    pub field: Field,
    pub editing_id: Option<i64>,
    /// Inline validation message (currently: only "empty title"), cleared
    /// on the next edit to the title.
    pub error: Option<String>,
}

/// The validated result of a submitted form, for `App` to persist.
#[derive(Debug, Clone)]
pub struct TaskDraft {
    pub title: String,
    pub body: String,
    pub priority: Priority,
    pub tags: Vec<String>,
    pub editing_id: Option<i64>,
    pub kind: FormKind,
}

const TASK_FIELDS: [Field; 4] = [Field::Title, Field::Priority, Field::Tags, Field::Body];
const NOTE_FIELDS: [Field; 2] = [Field::Title, Field::Body];

impl FormState {
    pub fn new_task(status: Status) -> Self {
        FormState {
            kind: FormKind::NewTask(status),
            title: String::new(),
            cursor: 0,
            priority: Priority::default(),
            tags: Vec::new(),
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
            priority: Priority::default(),
            tags: Vec::new(),
            body: String::new(),
            field: Field::Title,
            editing_id: None,
            error: None,
        }
    }

    /// Prefills an edit-task form. `title`/`body`/`priority` are the
    /// task's current values.
    pub fn edit_task(id: i64, title: String, body: String, priority: Priority) -> Self {
        let cursor = title.len();
        FormState {
            kind: FormKind::EditTask,
            title,
            cursor,
            priority,
            tags: Vec::new(),
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
            priority: Priority::default(),
            tags: Vec::new(),
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

    // --- title cursor model (UTF-8 safe) --------------------------------

    fn prev_char_boundary(&self, idx: usize) -> usize {
        if idx == 0 {
            return 0;
        }
        let mut i = idx - 1;
        while i > 0 && !self.title.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    fn next_char_boundary(&self, idx: usize) -> usize {
        if idx >= self.title.len() {
            return self.title.len();
        }
        let mut i = idx + 1;
        while i < self.title.len() && !self.title.is_char_boundary(i) {
            i += 1;
        }
        i
    }

    pub fn insert_char(&mut self, c: char) {
        self.title.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.prev_char_boundary(self.cursor);
        self.title.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn delete_forward(&mut self) {
        if self.cursor >= self.title.len() {
            return;
        }
        let end = self.next_char_boundary(self.cursor);
        self.title.replace_range(self.cursor..end, "");
    }

    pub fn cursor_left(&mut self) {
        self.cursor = self.prev_char_boundary(self.cursor);
    }

    pub fn cursor_right(&mut self) {
        self.cursor = self.next_char_boundary(self.cursor);
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
            Field::Priority => self.handle_priority_key(key),
            Field::Tags => self.handle_tags_key(key),
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

    fn handle_tags_key(&mut self, key: KeyEvent) -> PopupOutcome {
        if key.code == KeyCode::Enter {
            let mut items: Vec<SelectItem> = self
                .tags
                .iter()
                .enumerate()
                .map(|(i, t)| SelectItem { id: i as i64, label: t.clone() })
                .collect();
            items.push(SelectItem { id: -1, label: "+ new tag…".to_string() });
            return PopupOutcome::Push(Box::new(Popup::Dropdown(DropdownState {
                title: "Tags".to_string(),
                items,
                selected: 0,
                target: DropdownTarget::Tag,
            })));
        }
        PopupOutcome::Consumed
    }

    /// Applies a value received from a child popup (a priority/tag
    /// dropdown submitting back into this form). Only meaningful while
    /// `self.field` is `Priority` or `Tags`, since those are the only
    /// fields that push a child popup.
    pub fn receive(&mut self, value: PopupValue) {
        let PopupValue::Selected(item) = value else {
            return;
        };
        match self.field {
            Field::Priority => {
                if let Some(p) = Priority::ALL.iter().find(|p| **p as i64 == item.id) {
                    self.priority = *p;
                }
            }
            // Tag persistence is out of scope for this phase (see report);
            // this only tracks the label locally.
            Field::Tags if item.id == -1 && !self.tags.contains(&item.label) => {
                self.tags.push(item.label);
            }
            _ => {}
        }
    }

    fn try_submit(&mut self) -> PopupOutcome {
        if self.title.trim().is_empty() {
            self.error = Some("title cannot be empty".to_string());
            return PopupOutcome::Consumed;
        }
        PopupOutcome::Submit(PopupValue::Form(TaskDraft {
            title: self.title.clone(),
            body: self.body.clone(),
            priority: self.priority,
            tags: self.tags.clone(),
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
    fn tab_and_backtab_cycle_task_fields() {
        let mut f = FormState::new_task(Status::ToDo);
        assert_eq!(f.field, Field::Title);
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.field, Field::Priority);
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.field, Field::Tags);
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.field, Field::Body);
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
}
