//! The popup stack: dropdowns, the task/note form, confirmation prompts,
//! the passphrase prompt, the tag picker, one-line text prompts, and the
//! help screen. Every key press, while any popup is open, is routed to the
//! top of this stack rather than to the keymap.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::task_matches_query;
use crate::domain::dates;
use crate::domain::task::{Status, Tag, TagId, Task, TaskId};
use crate::ui::forms::{Field, FormState, TaskDraft};
use crate::ui::theme::Styles;

#[derive(Debug, Clone)]
pub enum Popup {
    Dropdown(DropdownState),
    Form(FormState),
    Confirm(ConfirmState),
    Passphrase(PassphraseState),
    TagPicker(TagPickerState),
    FormTagPicker(FormTagPickerState),
    TextPrompt(TextPromptState),
    Archive(ArchiveBrowserState),
    Help,
}

/// What handling one key press on the top popup should do to the stack.
#[derive(Debug, Clone)]
pub enum PopupOutcome {
    /// The key was handled; nothing else changes.
    Consumed,
    /// Pop this popup off the stack.
    Close,
    /// Push a new popup on top of this one.
    Push(Box<Popup>),
    /// This popup is done: pop it, and hand `PopupValue` either to the new
    /// top of the stack (via `receive`) or, if the stack is now empty, to
    /// the caller holding the stack.
    Submit(PopupValue),
    /// A tag picker in-place action: unlike `Submit`, this never pops the
    /// tag picker off the stack (a `Toggle`); the caller decides whether
    /// `New`/`Rename`/`Delete` replace it with a text prompt or confirm.
    TagPicker(TagPickerAction),
    /// The form-scoped tag picker's in-place action -- see
    /// `FormTagPickerAction`.
    FormTagPicker(FormTagPickerAction),
    /// The archive browser's Enter on task `TaskId`: the caller (`App`)
    /// pops the browser and pushes the edit form for it, fetched fresh
    /// from the store so its tags are current -- the browser's own key
    /// handler has no access to the store, the same reason
    /// `TagPickerAction`/`FormTagPickerAction` hand their New/Rename/
    /// Delete back to `App` rather than acting in place.
    OpenArchivedTask(TaskId),
}

#[derive(Debug, Clone)]
pub enum PopupValue {
    Selected(SelectItem),
    Form(TaskDraft),
    Confirmed(bool),
    Passphrase(String),
    /// A one-line `TextPrompt`'s submitted text (new-tag name, rename-tag
    /// name, or a quick-capture note body -- which of those it is comes
    /// from whatever `PendingPopupAction` the caller had recorded before
    /// pushing the prompt).
    Text(String),
}

#[derive(Debug, Clone)]
pub struct DropdownState {
    pub title: String,
    pub items: Vec<SelectItem>,
    pub selected: usize,
    pub target: DropdownTarget,
}

#[derive(Debug, Clone)]
pub struct SelectItem {
    pub id: i64,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropdownTarget {
    Column,
    Priority,
    Board,
    BoardMenu,
    BoardDelete,
    Status,
}

#[derive(Debug, Clone)]
pub struct ConfirmState {
    pub message: String,
}

/// Live state for a masked passphrase prompt: used both to unlock an
/// existing locked board and, twice in a row, to set a new one's
/// passphrase. `input` is the plaintext the user has typed so far -- it is
/// never rendered; only its length (as `•` characters) is.
#[derive(Debug, Clone)]
pub struct PassphraseState {
    pub prompt: String,
    pub input: String,
    pub error: Option<String>,
}

/// A one-line, unmasked text prompt: new-tag name, rename-tag name, and
/// note quick-capture all use this same popup, distinguished only by which
/// `PendingPopupAction` `App` recorded before pushing it.
#[derive(Debug, Clone)]
pub struct TextPromptState {
    pub prompt: String,
    pub input: String,
    pub error: Option<String>,
}

/// The board's tags for one task: navigable as `tags` rows plus a final
/// "+ new tag…" row. `applied` holds the ids currently on `task_id`, shown
/// with a `*` marker; Enter on a tag row toggles membership immediately
/// (persisted by `App`, which then refreshes `applied` in place) without
/// closing the picker, so several tags can be toggled in one sitting.
#[derive(Debug, Clone)]
pub struct TagPickerState {
    pub task_id: TaskId,
    pub tags: Vec<Tag>,
    pub applied: Vec<TagId>,
    pub selected: usize,
}

/// What a key press on the tag picker asks `App` to do. `Toggle` is
/// handled in place (the picker stays open); `New`/`Rename`/`Delete` ask
/// `App` to replace the picker with a text prompt or a confirmation.
#[derive(Debug, Clone, Copy)]
pub enum TagPickerAction {
    Toggle(TagId),
    New,
    Rename(TagId),
    Delete(TagId),
}

/// The board's tags for the currently open task/note form's Tags field:
/// navigable as name rows plus a trailing "+ new tag…" row, the same
/// feel as `TagPickerState` but scoped to the form's own in-memory
/// `Vec<String>` of tag names rather than a real task's stored tags --
/// nothing here is persisted to the store until the form itself is
/// submitted. `applied` is the subset of `all_tags` currently in the
/// form's `tags`, shown with a `*` marker.
#[derive(Debug, Clone)]
pub struct FormTagPickerState {
    pub all_tags: Vec<String>,
    pub applied: Vec<String>,
    pub selected: usize,
}

/// What a key press on the form-scoped tag picker asks `App` to do.
/// `Toggle` is handled in place, writing straight into the form beneath it
/// on the stack; `New` asks `App` to replace this picker with a one-line
/// text prompt, whose submitted name flows back into the form via
/// `Popup::receive` (the form is still underneath it, same as the
/// Status/Priority dropdowns).
#[derive(Debug, Clone, Copy)]
pub enum FormTagPickerAction {
    Toggle(usize),
    New,
}

/// The current board's archived (auto-hidden Done) tasks, loaded once when
/// the browser opens (`App::open_archive_browser`), already newest-
/// completion-first, and filtered live as the user types. `j`/`k`/arrows
/// are reserved for navigation -- the same trade-off every other list
/// popup in this module makes -- so any other character extends `filter`
/// instead. `selected` indexes into `visible()`, the current filtered
/// view, not into `tasks` directly; it is reset to 0 whenever the filter
/// text changes, since the filtered set's shape just changed under it.
#[derive(Debug, Clone)]
pub struct ArchiveBrowserState {
    pub tasks: Vec<Task>,
    pub filter: String,
    pub selected: usize,
}

impl ArchiveBrowserState {
    /// The tasks currently matching `filter` (case-insensitive substring
    /// over title, body, and tag names, via the same `task_matches_query`
    /// the board's own `/` search uses), in `tasks`' order.
    pub fn visible(&self) -> Vec<&Task> {
        if self.filter.is_empty() {
            self.tasks.iter().collect()
        } else {
            self.tasks.iter().filter(|t| task_matches_query(t, &self.filter)).collect()
        }
    }
}

impl Popup {
    pub fn handle_key(&mut self, key: KeyEvent) -> PopupOutcome {
        match self {
            Popup::Dropdown(d) => handle_dropdown_key(d, key),
            Popup::Form(f) => f.handle_key(key),
            Popup::Confirm(_) => handle_confirm_key(key),
            Popup::Passphrase(p) => handle_passphrase_key(p, key),
            Popup::TagPicker(t) => handle_tag_picker_key(t, key),
            Popup::FormTagPicker(t) => handle_form_tag_picker_key(t, key),
            Popup::TextPrompt(p) => handle_text_prompt_key(p, key),
            Popup::Archive(t) => handle_archive_key(t, key),
            Popup::Help => PopupOutcome::Close,
        }
    }

    /// Feeds a value submitted by a child popup into this one. Only a
    /// `Form` currently has children (its priority/status/tag dropdowns);
    /// the others ignore it. The tag picker's own new/rename/delete flow
    /// does not nest this way -- see `TagPickerAction`.
    pub fn receive(&mut self, value: PopupValue) {
        if let Popup::Form(f) = self {
            f.receive(value);
        }
    }
}

fn handle_dropdown_key(d: &mut DropdownState, key: KeyEvent) -> PopupOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
        return PopupOutcome::Close;
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if !d.items.is_empty() {
                d.selected = (d.selected + 1) % d.items.len();
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if !d.items.is_empty() {
                d.selected = if d.selected == 0 { d.items.len() - 1 } else { d.selected - 1 };
            }
        }
        // A lone 'g' already lands on the first item, so a second 'g'
        // (completing the vim "gg" sequence) is a harmless no-op.
        KeyCode::Char('g') => d.selected = 0,
        KeyCode::Char('G') => d.selected = d.items.len().saturating_sub(1),
        KeyCode::Enter => {
            return match d.items.get(d.selected) {
                Some(item) => PopupOutcome::Submit(PopupValue::Selected(item.clone())),
                None => PopupOutcome::Close,
            };
        }
        _ => {}
    }
    PopupOutcome::Consumed
}

fn handle_confirm_key(key: KeyEvent) -> PopupOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => PopupOutcome::Submit(PopupValue::Confirmed(true)),
        KeyCode::Char('n') | KeyCode::Esc => PopupOutcome::Submit(PopupValue::Confirmed(false)),
        KeyCode::Char('c') if ctrl => PopupOutcome::Submit(PopupValue::Confirmed(false)),
        _ => PopupOutcome::Consumed,
    }
}

fn handle_passphrase_key(p: &mut PassphraseState, key: KeyEvent) -> PopupOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
        return PopupOutcome::Close;
    }
    match key.code {
        KeyCode::Enter => return PopupOutcome::Submit(PopupValue::Passphrase(std::mem::take(&mut p.input))),
        KeyCode::Backspace => {
            p.input.pop();
        }
        KeyCode::Char(c) => p.input.push(c),
        _ => {}
    }
    PopupOutcome::Consumed
}

fn handle_text_prompt_key(p: &mut TextPromptState, key: KeyEvent) -> PopupOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
        return PopupOutcome::Close;
    }
    // Ctrl-U clears the line, Ctrl-W deletes the previous word (readline convention).
    if ctrl {
        match key.code {
            KeyCode::Char('u') => p.input.clear(),
            KeyCode::Char('w') => {
                let trimmed = p.input.trim_end_matches(' ').len();
                let cut = p.input[..trimmed].rfind(' ').map_or(0, |i| i + 1);
                p.input.truncate(cut);
            }
            _ => return PopupOutcome::Consumed,
        }
        p.error = None;
        return PopupOutcome::Consumed;
    }
    match key.code {
        KeyCode::Enter => return PopupOutcome::Submit(PopupValue::Text(std::mem::take(&mut p.input))),
        KeyCode::Backspace => {
            p.input.pop();
            p.error = None;
        }
        KeyCode::Char(c) => {
            p.input.push(c);
            p.error = None;
        }
        _ => {}
    }
    PopupOutcome::Consumed
}

/// `j/k`/`gg`/`G` navigate the tag rows plus the trailing "+ new tag…" row;
/// Enter toggles a tag (or, on the last row, starts the new-tag flow); `r`
/// renames and `d` deletes the highlighted tag (both no-ops on the "+ new
/// tag…" row, since it names no existing tag).
fn handle_tag_picker_key(t: &mut TagPickerState, key: KeyEvent) -> PopupOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
        return PopupOutcome::Close;
    }
    let len = t.tags.len() + 1;
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => t.selected = (t.selected + 1) % len,
        KeyCode::Char('k') | KeyCode::Up => {
            t.selected = if t.selected == 0 { len - 1 } else { t.selected - 1 };
        }
        KeyCode::Char('g') => t.selected = 0,
        KeyCode::Char('G') => t.selected = len - 1,
        KeyCode::Enter => {
            if t.selected >= t.tags.len() {
                return PopupOutcome::TagPicker(TagPickerAction::New);
            }
            return PopupOutcome::TagPicker(TagPickerAction::Toggle(t.tags[t.selected].id));
        }
        KeyCode::Char('r') if t.selected < t.tags.len() => {
            return PopupOutcome::TagPicker(TagPickerAction::Rename(t.tags[t.selected].id));
        }
        KeyCode::Char('d') if t.selected < t.tags.len() => {
            return PopupOutcome::TagPicker(TagPickerAction::Delete(t.tags[t.selected].id));
        }
        _ => {}
    }
    PopupOutcome::Consumed
}

/// `j/k`/`gg`/`G` navigate the tag-name rows plus the trailing "+ new
/// tag…" row; Enter toggles a row's membership in the form's own
/// `tags` (or, on the last row, starts the new-tag flow). No rename/delete:
/// those are the card-level picker's job, not the form's.
fn handle_form_tag_picker_key(t: &mut FormTagPickerState, key: KeyEvent) -> PopupOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
        return PopupOutcome::Close;
    }
    let len = t.all_tags.len() + 1;
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => t.selected = (t.selected + 1) % len,
        KeyCode::Char('k') | KeyCode::Up => {
            t.selected = if t.selected == 0 { len - 1 } else { t.selected - 1 };
        }
        KeyCode::Char('g') => t.selected = 0,
        KeyCode::Char('G') => t.selected = len - 1,
        KeyCode::Enter => {
            if t.selected >= t.all_tags.len() {
                return PopupOutcome::FormTagPicker(FormTagPickerAction::New);
            }
            return PopupOutcome::FormTagPicker(FormTagPickerAction::Toggle(t.selected));
        }
        _ => {}
    }
    PopupOutcome::Consumed
}

/// `j`/`k`/arrows navigate the currently filtered list (wrapping, same as
/// every other popup here); any other character is appended to the filter
/// instead, so `j`/`k`/`g`/`G` cannot themselves be typed into it. Backspace
/// edits the filter. Enter opens the highlighted task -- see
/// `PopupOutcome::OpenArchivedTask`. The filter changing always resets
/// `selected` to 0, since the filtered set's shape just changed under it.
fn handle_archive_key(t: &mut ArchiveBrowserState, key: KeyEvent) -> PopupOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
        return PopupOutcome::Close;
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            let len = t.visible().len();
            if len > 0 {
                t.selected = (t.selected + 1) % len;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            let len = t.visible().len();
            if len > 0 {
                t.selected = if t.selected == 0 { len - 1 } else { t.selected - 1 };
            }
        }
        KeyCode::Backspace => {
            t.filter.pop();
            t.selected = 0;
        }
        KeyCode::Enter => {
            if let Some(task) = t.visible().get(t.selected) {
                return PopupOutcome::OpenArchivedTask(task.id);
            }
        }
        KeyCode::Char(c) if !ctrl => {
            t.filter.push(c);
            t.selected = 0;
        }
        _ => {}
    }
    PopupOutcome::Consumed
}

/// Returns a `width` x `height` rectangle centred within `area`, clamped so
/// it always fits inside `area` even when `area` is smaller than the
/// requested size.
pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect { x, y, width: w, height: h }
}

/// Renders the whole popup stack, bottom to top, over a dimmed background.
/// Draws nothing when the stack is empty.
pub fn render(frame: &mut Frame, area: Rect, popups: &[Popup], styles: &Styles) {
    if popups.is_empty() {
        return;
    }
    let dim = Block::default().style(styles.default.add_modifier(Modifier::DIM));
    frame.render_widget(dim, area);

    for popup in popups {
        render_one(frame, area, popup, styles);
    }
}

fn render_one(frame: &mut Frame, area: Rect, popup: &Popup, styles: &Styles) {
    match popup {
        Popup::Dropdown(d) => render_dropdown(frame, area, d, styles),
        Popup::Form(f) => render_form(frame, area, f, styles),
        Popup::Confirm(c) => render_confirm(frame, area, c, styles),
        Popup::Passphrase(p) => render_passphrase(frame, area, p, styles),
        Popup::TagPicker(t) => render_tag_picker(frame, area, t, styles),
        Popup::FormTagPicker(t) => render_form_tag_picker(frame, area, t, styles),
        Popup::TextPrompt(p) => render_text_prompt(frame, area, p, styles),
        Popup::Archive(t) => render_archive_browser(frame, area, t, styles),
        Popup::Help => render_help(frame, area, styles),
    }
}

fn render_dropdown(frame: &mut Frame, area: Rect, d: &DropdownState, styles: &Styles) {
    let width = d
        .items
        .iter()
        .map(|i| i.label.chars().count() as u16 + 4)
        .max()
        .unwrap_or(10)
        .max(d.title.chars().count() as u16 + 4)
        .max(16);
    let height = (d.items.len() as u16 + 2).max(3);
    let rect = centered_rect(width, height, area);

    let items: Vec<ListItem> = d
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let style = if i == d.selected { styles.selection } else { styles.default };
            ListItem::new(Line::from(Span::styled(item.label.clone(), style)))
        })
        .collect();

    let block = Block::bordered().title(Line::from(format!(" {} ", d.title))).border_style(styles.border);
    frame.render_widget(Clear, rect);
    frame.render_widget(List::new(items).block(block), rect);
}

fn status_text(status: Status) -> &'static str {
    match status {
        Status::ToDo => "ToDo",
        Status::Doing => "Doing",
        Status::Done => "Done",
    }
}

fn field_label(field: Field, is_note: bool) -> &'static str {
    match field {
        Field::Title => "Title",
        Field::Status => "Status",
        Field::Priority => "Priority",
        Field::Tags => "Tags",
        Field::Start => "Start",
        Field::TimeExpected => "TimeExpected",
        Field::Deadline => "Deadline",
        Field::Body => {
            if is_note {
                "Body"
            } else {
                "Body (Enter to open $EDITOR)"
            }
        }
    }
}

fn render_form(frame: &mut Frame, area: Rect, f: &FormState, styles: &Styles) {
    let width = 60u16.min(area.width.max(10));
    let height = if f.is_note() { 8u16 } else { 15u16 }.min(area.height.max(6));
    let rect = centered_rect(width, height, area);

    let title = match f.kind {
        crate::ui::forms::FormKind::NewTask(_) => "New task",
        crate::ui::forms::FormKind::EditTask => "Edit task",
        crate::ui::forms::FormKind::NewNote => "New note",
        crate::ui::forms::FormKind::EditNote => "Edit note",
    };
    let block = Block::bordered().title(Line::from(format!(" {title} "))).border_style(styles.border);

    let mut lines: Vec<Line> = Vec::new();
    let field_style = |field: Field| if f.field == field { styles.selection } else { styles.default };

    lines.push(Line::from(vec![
        Span::styled(format!("{}: ", field_label(Field::Title, f.is_note())), field_style(Field::Title)),
        Span::styled(f.title.clone(), styles.default),
    ]));
    if !f.is_note() {
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", field_label(Field::Status, false)), field_style(Field::Status)),
            Span::styled(status_text(f.status), styles.default),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", field_label(Field::Priority, false)), field_style(Field::Priority)),
            Span::styled(f.priority.as_str(), styles.default),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", field_label(Field::Tags, false)), field_style(Field::Tags)),
            Span::styled(f.tags.join(", "), styles.default),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", field_label(Field::Start, false)), field_style(Field::Start)),
            Span::styled(f.start_text.clone(), styles.default),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", field_label(Field::TimeExpected, false)), field_style(Field::TimeExpected)),
            Span::styled(f.time_expected_text.clone(), styles.default),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", field_label(Field::Deadline, false)), field_style(Field::Deadline)),
            Span::styled(f.deadline_text.clone(), styles.default),
        ]));
    }
    lines.push(Line::from(Span::styled(
        field_label(Field::Body, f.is_note()),
        field_style(Field::Body),
    )));
    let body_preview = f.body.lines().next().unwrap_or("");
    lines.push(Line::from(Span::styled(body_preview, styles.default)));
    if let Some(err) = &f.error {
        lines.push(Line::from(Span::styled(err.clone(), styles.priority_urgent)));
    }

    frame.render_widget(Clear, rect);
    frame.render_widget(block.clone(), rect);
    let inner = block.inner(rect);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_confirm(frame: &mut Frame, area: Rect, c: &ConfirmState, styles: &Styles) {
    let width = (c.message.chars().count() as u16 + 4).max(20).min(area.width.max(4));
    let height = area.height.min(3);
    let rect = centered_rect(width, height, area);
    let block = Block::bordered().title(Line::from(" Confirm ")).border_style(styles.border);
    frame.render_widget(Clear, rect);
    frame.render_widget(block.clone(), rect);
    let inner = block.inner(rect);
    frame.render_widget(Paragraph::new(Line::from(c.message.clone())), inner);
}

/// Renders the passphrase prompt with the typed passphrase masked as one
/// `•` per character -- the plaintext itself never appears in any styled
/// span here.
fn render_passphrase(frame: &mut Frame, area: Rect, p: &PassphraseState, styles: &Styles) {
    let masked: String = "•".repeat(p.input.chars().count());
    let width = (p.prompt.chars().count().max(masked.chars().count()) as u16 + 4)
        .max(24)
        .min(area.width.max(4));
    let height = if p.error.is_some() { 4 } else { 3 }.min(area.height.max(3));
    let rect = centered_rect(width, height, area);
    let block = Block::bordered().title(Line::from(format!(" {} ", p.prompt))).border_style(styles.border);
    frame.render_widget(Clear, rect);
    frame.render_widget(block.clone(), rect);
    let inner = block.inner(rect);

    let mut lines = vec![Line::from(masked)];
    if let Some(err) = &p.error {
        lines.push(Line::from(Span::styled(err.clone(), styles.priority_urgent)));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_text_prompt(frame: &mut Frame, area: Rect, p: &TextPromptState, styles: &Styles) {
    let width = (p.prompt.chars().count().max(p.input.chars().count()) as u16 + 4)
        .max(24)
        .min(area.width.max(4));
    let height = if p.error.is_some() { 4 } else { 3 }.min(area.height.max(3));
    let rect = centered_rect(width, height, area);
    let block = Block::bordered().title(Line::from(format!(" {} ", p.prompt))).border_style(styles.border);
    frame.render_widget(Clear, rect);
    frame.render_widget(block.clone(), rect);
    let inner = block.inner(rect);

    let mut lines = vec![Line::from(p.input.clone())];
    if let Some(err) = &p.error {
        lines.push(Line::from(Span::styled(err.clone(), styles.priority_urgent)));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_tag_picker(frame: &mut Frame, area: Rect, t: &TagPickerState, styles: &Styles) {
    let mut labels: Vec<String> = t
        .tags
        .iter()
        .map(|tag| {
            let marker = if t.applied.contains(&tag.id) { "* " } else { "  " };
            format!("{marker}{}", tag.name)
        })
        .collect();
    labels.push("  + new tag…".to_string());

    let title = " Tags (Enter toggle, r rename, d delete) ";
    let width = labels
        .iter()
        .map(|l| l.chars().count() as u16 + 4)
        .max()
        .unwrap_or(10)
        .max((title.chars().count() as u16) + 2)
        .max(20);
    let height = (labels.len() as u16 + 2).max(3);
    let rect = centered_rect(width, height, area);

    let items: Vec<ListItem> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let style = if i == t.selected { styles.selection } else { styles.default };
            ListItem::new(Line::from(Span::styled(label.clone(), style)))
        })
        .collect();

    let block = Block::bordered()
        .title(Line::from(title))
        .border_style(styles.border);
    frame.render_widget(Clear, rect);
    frame.render_widget(List::new(items).block(block), rect);
}

fn render_form_tag_picker(frame: &mut Frame, area: Rect, t: &FormTagPickerState, styles: &Styles) {
    let mut labels: Vec<String> = t
        .all_tags
        .iter()
        .map(|name| {
            let marker = if t.applied.contains(name) { "* " } else { "  " };
            format!("{marker}{name}")
        })
        .collect();
    labels.push("  + new tag…".to_string());

    let title = " Tags (Enter toggle) ";
    let width = labels
        .iter()
        .map(|l| l.chars().count() as u16 + 4)
        .max()
        .unwrap_or(10)
        .max((title.chars().count() as u16) + 2)
        .max(20);
    let height = (labels.len() as u16 + 2).max(3);
    let rect = centered_rect(width, height, area);

    let items: Vec<ListItem> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let style = if i == t.selected { styles.selection } else { styles.default };
            ListItem::new(Line::from(Span::styled(label.clone(), style)))
        })
        .collect();

    let block = Block::bordered().title(Line::from(title)).border_style(styles.border);
    frame.render_widget(Clear, rect);
    frame.render_widget(List::new(items).block(block), rect);
}

fn render_archive_browser(frame: &mut Frame, area: Rect, t: &ArchiveBrowserState, styles: &Styles) {
    let visible = t.visible();
    let title = if t.filter.is_empty() {
        " Archive ".to_string()
    } else {
        format!(" Archive — /{} ", t.filter)
    };
    let width = visible
        .iter()
        .map(|task| (task.title.chars().count() + 13) as u16)
        .max()
        .unwrap_or(10)
        .max(title.chars().count() as u16 + 2)
        .max(30)
        .min(area.width.max(4));
    let height = ((visible.len().max(1)) as u16 + 2).max(3).min(area.height.max(3));
    let rect = centered_rect(width, height, area);

    let items: Vec<ListItem> = if visible.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "— no matching archived tasks —",
            styles.default.add_modifier(Modifier::DIM),
        )))]
    } else {
        visible
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let date = task.completed_at.map(dates::format_date).unwrap_or_else(|| "?".to_string());
                let label = format!("{date}  {}", task.title);
                let style = if i == t.selected { styles.selection } else { styles.default };
                ListItem::new(Line::from(Span::styled(label, style)))
            })
            .collect()
    };

    let block = Block::bordered().title(Line::from(title)).border_style(styles.border);
    frame.render_widget(Clear, rect);
    frame.render_widget(List::new(items).block(block), rect);
}

fn render_help(frame: &mut Frame, area: Rect, styles: &Styles) {
    // Board, notes-pane and global bindings: a popup's own keys are already in the footer
    // while it is open. `Ctx::BoardArrows` is included here too -- the arrow-key
    // equivalents of Board's h/l/j/k, kept out of the footer (which only ever
    // asks for `Ctx::Board`) so they do not lengthen its hint line. Entries flow
    // into extra columns when the terminal is too short to list them in one.
    const COL_WIDTH: u16 = 23;
    let entries: Vec<String> = crate::keymap::BINDINGS
        .iter()
        .filter(|b| {
            matches!(
                b.ctx,
                crate::keymap::Ctx::Board
                    | crate::keymap::Ctx::BoardArrows
                    | crate::keymap::Ctx::Notes
                    | crate::keymap::Ctx::Global
            )
        })
        .map(|b| format!("{:<8} {:<13} ", b.keys, b.label))
        .collect();
    let max_rows = (area.height.saturating_sub(4) as usize).max(1);
    let cols = entries.len().div_ceil(max_rows).max(1);
    let rows = entries.len().div_ceil(cols);
    let rect = centered_rect(COL_WIDTH * cols as u16 + 2, rows as u16 + 2, area);
    let block = Block::bordered().title(Line::from(" Help ")).border_style(styles.border);
    frame.render_widget(Clear, rect);
    frame.render_widget(block.clone(), rect);
    let inner = block.inner(rect);
    let lines: Vec<Line> = (0..rows)
        .map(|r| {
            let row: String = (0..cols)
                .filter_map(|c| entries.get(c * rows + r))
                .map(String::as_str)
                .collect();
            Line::from(row)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::task::Status;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample_dropdown() -> DropdownState {
        DropdownState {
            title: "Column".to_string(),
            items: vec![
                SelectItem { id: 0, label: "ToDo".to_string() },
                SelectItem { id: 1, label: "Doing".to_string() },
                SelectItem { id: 2, label: "Done".to_string() },
            ],
            selected: 0,
            target: DropdownTarget::Column,
        }
    }

    fn sample_tag_picker() -> TagPickerState {
        TagPickerState {
            task_id: 1,
            tags: vec![
                Tag { id: 10, name: "home".to_string(), color: None },
                Tag { id: 11, name: "work".to_string(), color: None },
            ],
            applied: vec![10],
            selected: 0,
        }
    }

    #[test]
    fn centered_rect_stays_inside_small_area() {
        let area = Rect { x: 0, y: 0, width: 40, height: 10 };
        let r = centered_rect(60, 20, area);
        assert!(r.x + r.width <= area.width);
        assert!(r.y + r.height <= area.height);
    }

    #[test]
    fn centered_rect_stays_inside_large_area() {
        let area = Rect { x: 0, y: 0, width: 200, height: 60 };
        let r = centered_rect(60, 20, area);
        assert_eq!(r.width, 60);
        assert_eq!(r.height, 20);
        assert!(r.x + r.width <= area.width);
        assert!(r.y + r.height <= area.height);
    }

    #[test]
    fn dropdown_j_k_wrap_around() {
        let mut d = sample_dropdown();
        assert!(matches!(handle_dropdown_key(&mut d, key(KeyCode::Char('k'))), PopupOutcome::Consumed));
        assert_eq!(d.selected, 2); // wrapped up from 0
        handle_dropdown_key(&mut d, key(KeyCode::Char('j')));
        assert_eq!(d.selected, 0); // wrapped down from 2
    }

    #[test]
    fn dropdown_up_down_arrows_navigate_the_same_as_j_k() {
        let mut d = sample_dropdown();
        assert!(matches!(handle_dropdown_key(&mut d, key(KeyCode::Up)), PopupOutcome::Consumed));
        assert_eq!(d.selected, 2); // wrapped up from 0, same as 'k'
        handle_dropdown_key(&mut d, key(KeyCode::Down));
        assert_eq!(d.selected, 0); // wrapped down from 2, same as 'j'
    }

    #[test]
    fn dropdown_gg_and_g_jump_ends() {
        let mut d = sample_dropdown();
        d.selected = 1;
        handle_dropdown_key(&mut d, key(KeyCode::Char('G')));
        assert_eq!(d.selected, 2);
        handle_dropdown_key(&mut d, key(KeyCode::Char('g')));
        assert_eq!(d.selected, 0);
    }

    #[test]
    fn dropdown_enter_submits_selected_item() {
        let mut d = sample_dropdown();
        d.selected = 1;
        match handle_dropdown_key(&mut d, key(KeyCode::Enter)) {
            PopupOutcome::Submit(PopupValue::Selected(item)) => assert_eq!(item.label, "Doing"),
            other => panic!("expected Submit(Selected), got {other:?}"),
        }
    }

    #[test]
    fn dropdown_esc_closes() {
        let mut d = sample_dropdown();
        assert!(matches!(handle_dropdown_key(&mut d, key(KeyCode::Esc)), PopupOutcome::Close));
    }

    #[test]
    fn confirm_y_and_n_and_esc() {
        assert!(matches!(
            handle_confirm_key(key(KeyCode::Char('y'))),
            PopupOutcome::Submit(PopupValue::Confirmed(true))
        ));
        assert!(matches!(
            handle_confirm_key(key(KeyCode::Char('n'))),
            PopupOutcome::Submit(PopupValue::Confirmed(false))
        ));
        assert!(matches!(
            handle_confirm_key(key(KeyCode::Esc)),
            PopupOutcome::Submit(PopupValue::Confirmed(false))
        ));
    }

    #[test]
    fn passphrase_chars_accumulate_and_backspace_removes() {
        let mut p = PassphraseState { prompt: "Passphrase".to_string(), input: String::new(), error: None };
        handle_passphrase_key(&mut p, key(KeyCode::Char('h')));
        handle_passphrase_key(&mut p, key(KeyCode::Char('i')));
        assert_eq!(p.input, "hi");
        handle_passphrase_key(&mut p, key(KeyCode::Backspace));
        assert_eq!(p.input, "h");
    }

    #[test]
    fn passphrase_enter_submits_and_clears_input() {
        let mut p = PassphraseState { prompt: "Passphrase".to_string(), input: "secret".to_string(), error: None };
        match handle_passphrase_key(&mut p, key(KeyCode::Enter)) {
            PopupOutcome::Submit(PopupValue::Passphrase(pw)) => assert_eq!(pw, "secret"),
            other => panic!("expected Submit(Passphrase(..)), got {other:?}"),
        }
        assert_eq!(p.input, "");
    }

    #[test]
    fn passphrase_esc_closes() {
        let mut p = PassphraseState { prompt: "Passphrase".to_string(), input: "secret".to_string(), error: None };
        assert!(matches!(handle_passphrase_key(&mut p, key(KeyCode::Esc)), PopupOutcome::Close));
    }

    #[test]
    fn text_prompt_ctrl_u_clears_input() {
        let mut p = TextPromptState { prompt: "New tag name".to_string(), input: "personal".to_string(), error: None };
        let outcome = handle_text_prompt_key(&mut p, KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(matches!(outcome, PopupOutcome::Consumed));
        assert_eq!(p.input, "");
    }

    #[test]
    fn text_prompt_ctrl_w_deletes_previous_word() {
        let mut p = TextPromptState { prompt: "New tag name".to_string(), input: "foo bar".to_string(), error: None };
        let outcome = handle_text_prompt_key(&mut p, KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert!(matches!(outcome, PopupOutcome::Consumed));
        assert_eq!(p.input, "foo ");
    }

    #[test]
    fn text_prompt_ctrl_x_does_not_insert_char() {
        let mut p = TextPromptState { prompt: "New tag name".to_string(), input: "test".to_string(), error: None };
        let outcome = handle_text_prompt_key(&mut p, KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(matches!(outcome, PopupOutcome::Consumed));
        assert_eq!(p.input, "test"); // unchanged
    }

    #[test]
    fn text_prompt_enter_submits_typed_text() {
        let mut p = TextPromptState { prompt: "New tag name".to_string(), input: "urgent".to_string(), error: None };
        match handle_text_prompt_key(&mut p, key(KeyCode::Enter)) {
            PopupOutcome::Submit(PopupValue::Text(s)) => assert_eq!(s, "urgent"),
            other => panic!("expected Submit(Text(..)), got {other:?}"),
        }
    }

    #[test]
    fn text_prompt_esc_closes() {
        let mut p = TextPromptState { prompt: "New tag name".to_string(), input: String::new(), error: None };
        assert!(matches!(handle_text_prompt_key(&mut p, key(KeyCode::Esc)), PopupOutcome::Close));
    }

    // --- tag picker --------------------------------------------------------

    #[test]
    fn tag_picker_enter_on_tag_row_toggles() {
        let mut t = sample_tag_picker();
        match handle_tag_picker_key(&mut t, key(KeyCode::Enter)) {
            PopupOutcome::TagPicker(TagPickerAction::Toggle(id)) => assert_eq!(id, 10),
            other => panic!("expected TagPicker(Toggle), got {other:?}"),
        }
    }

    #[test]
    fn tag_picker_enter_on_new_row_starts_new_tag_flow() {
        let mut t = sample_tag_picker();
        t.selected = t.tags.len(); // the trailing "+ new tag…" row
        match handle_tag_picker_key(&mut t, key(KeyCode::Enter)) {
            PopupOutcome::TagPicker(TagPickerAction::New) => {}
            other => panic!("expected TagPicker(New), got {other:?}"),
        }
    }

    #[test]
    fn tag_picker_r_renames_highlighted_tag() {
        let mut t = sample_tag_picker();
        t.selected = 1;
        match handle_tag_picker_key(&mut t, key(KeyCode::Char('r'))) {
            PopupOutcome::TagPicker(TagPickerAction::Rename(id)) => assert_eq!(id, 11),
            other => panic!("expected TagPicker(Rename), got {other:?}"),
        }
    }

    #[test]
    fn tag_picker_d_deletes_highlighted_tag() {
        let mut t = sample_tag_picker();
        match handle_tag_picker_key(&mut t, key(KeyCode::Char('d'))) {
            PopupOutcome::TagPicker(TagPickerAction::Delete(id)) => assert_eq!(id, 10),
            other => panic!("expected TagPicker(Delete), got {other:?}"),
        }
    }

    #[test]
    fn tag_picker_r_and_d_are_noops_on_the_new_tag_row() {
        let mut t = sample_tag_picker();
        t.selected = t.tags.len();
        assert!(matches!(handle_tag_picker_key(&mut t, key(KeyCode::Char('r'))), PopupOutcome::Consumed));
        assert!(matches!(handle_tag_picker_key(&mut t, key(KeyCode::Char('d'))), PopupOutcome::Consumed));
    }

    #[test]
    fn tag_picker_navigation_wraps_across_the_new_tag_row() {
        let mut t = sample_tag_picker();
        handle_tag_picker_key(&mut t, key(KeyCode::Char('k')));
        assert_eq!(t.selected, 2); // wraps to the "+ new tag…" row
        handle_tag_picker_key(&mut t, key(KeyCode::Char('j')));
        assert_eq!(t.selected, 0);
    }

    #[test]
    fn tag_picker_esc_closes() {
        let mut t = sample_tag_picker();
        assert!(matches!(handle_tag_picker_key(&mut t, key(KeyCode::Esc)), PopupOutcome::Close));
    }

    // --- form-scoped tag picker ---------------------------------------------

    fn sample_form_tag_picker() -> FormTagPickerState {
        FormTagPickerState {
            all_tags: vec!["home".to_string(), "work".to_string()],
            applied: vec!["home".to_string()],
            selected: 0,
        }
    }

    #[test]
    fn form_tag_picker_enter_on_tag_row_toggles() {
        let mut t = sample_form_tag_picker();
        match handle_form_tag_picker_key(&mut t, key(KeyCode::Enter)) {
            PopupOutcome::FormTagPicker(FormTagPickerAction::Toggle(idx)) => assert_eq!(idx, 0),
            other => panic!("expected FormTagPicker(Toggle), got {other:?}"),
        }
    }

    #[test]
    fn form_tag_picker_enter_on_new_row_starts_new_tag_flow() {
        let mut t = sample_form_tag_picker();
        t.selected = t.all_tags.len();
        match handle_form_tag_picker_key(&mut t, key(KeyCode::Enter)) {
            PopupOutcome::FormTagPicker(FormTagPickerAction::New) => {}
            other => panic!("expected FormTagPicker(New), got {other:?}"),
        }
    }

    #[test]
    fn form_tag_picker_navigation_wraps_across_the_new_tag_row() {
        let mut t = sample_form_tag_picker();
        handle_form_tag_picker_key(&mut t, key(KeyCode::Char('k')));
        assert_eq!(t.selected, 2);
        handle_form_tag_picker_key(&mut t, key(KeyCode::Char('j')));
        assert_eq!(t.selected, 0);
    }

    #[test]
    fn form_tag_picker_esc_closes() {
        let mut t = sample_form_tag_picker();
        assert!(matches!(handle_form_tag_picker_key(&mut t, key(KeyCode::Esc)), PopupOutcome::Close));
    }

    // --- stack-nesting behaviour (the highest-risk logic) ----------------
    //
    // These drive a small local stack the same way `App` does, to
    // table-test every combination of "which popup is on top gets the
    // key, and Submit/Close move exactly one level".

    fn apply(stack: &mut Vec<Popup>, outcome: PopupOutcome) {
        match outcome {
            PopupOutcome::Consumed => {}
            PopupOutcome::Close => {
                stack.pop();
            }
            PopupOutcome::Push(p) => stack.push(*p),
            PopupOutcome::Submit(value) => {
                stack.pop();
                if let Some(top) = stack.last_mut() {
                    top.receive(value);
                }
            }
            PopupOutcome::TagPicker(_) => {}
            PopupOutcome::FormTagPicker(_) => {}
            PopupOutcome::OpenArchivedTask(_) => {}
        }
    }

    #[test]
    fn dropdown_over_form_routes_keys_to_dropdown_only() {
        let mut stack: Vec<Popup> = vec![Popup::Form(FormState::new_task(Status::ToDo))];
        // Open the priority dropdown from the form.
        if let Popup::Form(f) = stack.last_mut().unwrap() {
            f.field = Field::Priority;
        }
        let outcome = stack.last_mut().unwrap().handle_key(key(KeyCode::Enter));
        apply(&mut stack, outcome);
        assert_eq!(stack.len(), 2);
        assert!(matches!(stack.last().unwrap(), Popup::Dropdown(_)));

        // Keys now go to the dropdown, not the form underneath.
        let outcome = stack.last_mut().unwrap().handle_key(key(KeyCode::Char('j')));
        apply(&mut stack, outcome);
        assert_eq!(stack.len(), 2);
    }

    #[test]
    fn esc_closes_only_the_dropdown_and_leaves_form_open() {
        let mut stack: Vec<Popup> = vec![Popup::Form(FormState::new_task(Status::ToDo))];
        if let Popup::Form(f) = stack.last_mut().unwrap() {
            f.field = Field::Priority;
        }
        let outcome = stack.last_mut().unwrap().handle_key(key(KeyCode::Enter));
        apply(&mut stack, outcome);
        assert_eq!(stack.len(), 2);

        let outcome = stack.last_mut().unwrap().handle_key(key(KeyCode::Esc));
        apply(&mut stack, outcome);
        assert_eq!(stack.len(), 1);
        assert!(matches!(stack.last().unwrap(), Popup::Form(_)));
    }

    #[test]
    fn enter_on_dropdown_feeds_value_into_forms_field() {
        let mut stack: Vec<Popup> = vec![Popup::Form(FormState::new_task(Status::ToDo))];
        if let Popup::Form(f) = stack.last_mut().unwrap() {
            f.field = Field::Priority;
        }
        let outcome = stack.last_mut().unwrap().handle_key(key(KeyCode::Enter));
        apply(&mut stack, outcome);

        // Move the dropdown's selection to Urgent, then Enter.
        if let Popup::Dropdown(d) = stack.last_mut().unwrap() {
            let idx = d.items.iter().position(|i| i.label == "urgent").unwrap();
            d.selected = idx;
        }
        let outcome = stack.last_mut().unwrap().handle_key(key(KeyCode::Enter));
        apply(&mut stack, outcome);

        assert_eq!(stack.len(), 1);
        match stack.last().unwrap() {
            Popup::Form(f) => assert_eq!(f.priority, crate::domain::task::Priority::Urgent),
            _ => panic!("expected the form to still be on top"),
        }
    }

    #[test]
    fn submit_with_empty_stack_after_pop_yields_no_receiver() {
        let mut stack: Vec<Popup> = vec![Popup::Confirm(ConfirmState { message: "delete?".to_string() })];
        let outcome = stack.last_mut().unwrap().handle_key(key(KeyCode::Char('y')));
        apply(&mut stack, outcome);
        assert!(stack.is_empty());
    }

    #[test]
    fn help_closes_on_any_key() {
        let mut stack: Vec<Popup> = vec![Popup::Help];
        let outcome = stack.last_mut().unwrap().handle_key(key(KeyCode::Char('x')));
        apply(&mut stack, outcome);
        assert!(stack.is_empty());
    }

    fn help_text(width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let styles = crate::ui::theme::resolve(&crate::config::Theme::default(), crate::ui::theme::ColorDepth::TrueColor);
        terminal
            .draw(|f| render_help(f, f.area(), &styles))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn help_lists_board_notes_and_global_bindings() {
        let text = help_text(140, 45);
        for b in crate::keymap::BINDINGS.iter().filter(|b| {
            matches!(
                b.ctx,
                crate::keymap::Ctx::Board
                    | crate::keymap::Ctx::BoardArrows
                    | crate::keymap::Ctx::Notes
                    | crate::keymap::Ctx::Global
            )
        }) {
            assert!(text.contains(b.label), "help is missing {:?}", b.keys);
        }
        assert!(text.contains("archive"));
    }

    #[test]
    fn help_lists_the_arrow_key_equivalents_of_h_l_j_k() {
        let text = help_text(140, 45);
        assert!(text.contains('\u{2190}'), "help is missing the left-arrow hint:\n{text}");
        assert!(text.contains('\u{2192}'), "help is missing the right-arrow hint:\n{text}");
        assert!(text.contains('\u{2191}'), "help is missing the up-arrow hint:\n{text}");
        assert!(text.contains('\u{2193}'), "help is missing the down-arrow hint:\n{text}");
    }

    #[test]
    fn help_flows_into_columns_on_a_short_terminal() {
        let text = help_text(140, 12);
        assert!(text.contains("archive"), "archive cut off on a short terminal:\n{text}");
    }

    // --- archive browser ----------------------------------------------------

    fn sample_task(id: TaskId, title: &str, completed_at: Option<i64>) -> Task {
        Task {
            id,
            board_id: None,
            title: title.to_string(),
            body: String::new(),
            status: Status::Done,
            priority: crate::domain::task::Priority::Normal,
            position: 0,
            created_at: 0,
            updated_at: 0,
            start_date: None,
            time_expected: None,
            deadline: None,
            deadline_notified_at: None,
            completed_at,
            tags: Vec::new(),
        }
    }

    fn sample_archive() -> ArchiveBrowserState {
        ArchiveBrowserState {
            tasks: vec![
                sample_task(1, "buy milk", Some(300)),
                sample_task(2, "call bank", Some(200)),
                sample_task(3, "buy eggs", Some(100)),
            ],
            filter: String::new(),
            selected: 0,
        }
    }

    #[test]
    fn archive_visible_is_unfiltered_when_filter_is_empty() {
        let a = sample_archive();
        assert_eq!(a.visible().len(), 3);
    }

    #[test]
    fn archive_filter_narrows_by_title_case_insensitively() {
        let mut a = sample_archive();
        a.filter = "BUY".to_string();
        let titles: Vec<&str> = a.visible().iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["buy milk", "buy eggs"]);
    }

    #[test]
    fn archive_filter_matches_body_and_tags_too() {
        let mut a = sample_archive();
        a.tasks[1].body = "ask about the mortgage".to_string();
        a.filter = "mortgage".to_string();
        assert_eq!(a.visible().len(), 1);
        assert_eq!(a.visible()[0].id, 2);

        a.filter = "nonsense".to_string();
        assert!(a.visible().is_empty());
    }

    #[test]
    fn archive_typing_appends_to_filter_and_resets_selection() {
        let mut a = sample_archive();
        a.selected = 2;
        let outcome = handle_archive_key(&mut a, key(KeyCode::Char('b')));
        assert!(matches!(outcome, PopupOutcome::Consumed));
        assert_eq!(a.filter, "b");
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn archive_backspace_edits_filter_and_resets_selection() {
        let mut a = sample_archive();
        a.filter = "buy".to_string();
        a.selected = 1;
        handle_archive_key(&mut a, key(KeyCode::Backspace));
        assert_eq!(a.filter, "bu");
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn archive_j_k_do_not_reach_the_filter_and_wrap_around() {
        let mut a = sample_archive();
        handle_archive_key(&mut a, key(KeyCode::Char('j')));
        assert_eq!(a.selected, 1);
        assert_eq!(a.filter, "", "j must navigate, not type");
        handle_archive_key(&mut a, key(KeyCode::Char('k')));
        assert_eq!(a.selected, 0);
        handle_archive_key(&mut a, key(KeyCode::Char('k')));
        assert_eq!(a.selected, 2, "k from the top wraps to the last row");
    }

    #[test]
    fn archive_arrows_navigate_the_same_as_j_k_and_do_not_reach_the_filter() {
        let mut a = sample_archive();
        handle_archive_key(&mut a, key(KeyCode::Down));
        assert_eq!(a.selected, 1);
        assert_eq!(a.filter, "", "Down must navigate, not type");
        handle_archive_key(&mut a, key(KeyCode::Up));
        assert_eq!(a.selected, 0);
        handle_archive_key(&mut a, key(KeyCode::Up));
        assert_eq!(a.selected, 2, "Up from the top wraps to the last row");
    }

    #[test]
    fn archive_enter_opens_the_selected_visible_task() {
        let mut a = sample_archive();
        a.filter = "call".to_string();
        match handle_archive_key(&mut a, key(KeyCode::Enter)) {
            PopupOutcome::OpenArchivedTask(id) => assert_eq!(id, 2),
            other => panic!("expected OpenArchivedTask, got {other:?}"),
        }
    }

    #[test]
    fn archive_esc_closes() {
        let mut a = sample_archive();
        assert!(matches!(handle_archive_key(&mut a, key(KeyCode::Esc)), PopupOutcome::Close));
    }

    #[test]
    fn archive_enter_on_empty_filtered_list_does_nothing() {
        let mut a = sample_archive();
        a.filter = "no such task".to_string();
        assert!(matches!(handle_archive_key(&mut a, key(KeyCode::Enter)), PopupOutcome::Consumed));
    }
}
