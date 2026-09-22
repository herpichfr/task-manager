//! The popup stack: dropdowns, the task/note form, confirmation prompts,
//! the passphrase prompt, and the help screen. Every key press, while any
//! popup is open, is routed to the top of this stack rather than to the
//! keymap.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::ui::forms::{Field, FormState, TaskDraft};
use crate::ui::theme::Styles;

#[derive(Debug, Clone)]
pub enum Popup {
    Dropdown(DropdownState),
    Form(FormState),
    Confirm(ConfirmState),
    Passphrase(PassphraseState),
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
}

#[derive(Debug, Clone)]
pub enum PopupValue {
    Selected(SelectItem),
    Form(TaskDraft),
    Confirmed(bool),
    Passphrase(String),
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
    Tag,
    Board,
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

impl Popup {
    pub fn handle_key(&mut self, key: KeyEvent) -> PopupOutcome {
        match self {
            Popup::Dropdown(d) => handle_dropdown_key(d, key),
            Popup::Form(f) => f.handle_key(key),
            Popup::Confirm(_) => handle_confirm_key(key),
            Popup::Passphrase(p) => handle_passphrase_key(p, key),
            Popup::Help => PopupOutcome::Close,
        }
    }

    /// Feeds a value submitted by a child popup into this one. Only a
    /// `Form` currently has children (its priority/tag dropdowns); the
    /// others ignore it.
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

fn field_label(field: Field, is_note: bool) -> &'static str {
    match field {
        Field::Title => "Title",
        Field::Priority => "Priority",
        Field::Tags => "Tags",
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
    let height = if f.is_note() { 8u16 } else { 10u16 }.min(area.height.max(6));
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
            Span::styled(format!("{}: ", field_label(Field::Priority, false)), field_style(Field::Priority)),
            Span::styled(f.priority.as_str(), styles.default),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("{}: ", field_label(Field::Tags, false)), field_style(Field::Tags)),
            Span::styled(f.tags.join(", "), styles.default),
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

fn render_help(frame: &mut Frame, area: Rect, styles: &Styles) {
    let rect = centered_rect(50, 12, area);
    let block = Block::bordered().title(Line::from(" Help ")).border_style(styles.border);
    frame.render_widget(Clear, rect);
    frame.render_widget(block.clone(), rect);
    let inner = block.inner(rect);
    let lines: Vec<Line> = crate::keymap::BINDINGS
        .iter()
        .map(|b| Line::from(format!("{:<8} {}", b.keys, b.label)))
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
}
