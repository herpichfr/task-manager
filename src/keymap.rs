//! Single source of truth for key handling: the same `BINDINGS` table backs
//! both key dispatch (`resolve`) and the footer's key-hint bar
//! (`hints_for`), so the two can never drift apart.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::actions::Action;
use crate::app::Pane;
use crate::mode::{Mode, PendingSeq};

/// One key binding: its display form, its footer label, its importance
/// (`rank`, 0 = most important: shown first, dropped last when the footer
/// is tight on space), and the context(s) it is shown in.
#[derive(Debug, Clone, Copy)]
pub struct Binding {
    pub keys: &'static str,
    pub label: &'static str,
    pub rank: u8,
    pub ctx: Ctx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    Board,
    Notes,
    Global,
    Help,
    Form,
    Dropdown,
    Confirm,
    Passphrase,
    TextPrompt,
    TagPicker,
}

pub const BINDINGS: &[Binding] = &[
    // --- Board: navigation (live in Phase 4) --------------------------
    Binding { keys: "h/l", label: "col", rank: 0, ctx: Ctx::Board },
    Binding { keys: "j/k", label: "move", rank: 0, ctx: Ctx::Board },
    Binding { keys: "gg/G", label: "top/bottom", rank: 2, ctx: Ctx::Board },
    Binding { keys: "^d/^u", label: "half pg", rank: 3, ctx: Ctx::Board },
    // --- Board: task actions (live in Phase 5) ------------------------
    Binding { keys: "a/i", label: "new task", rank: 1, ctx: Ctx::Board },
    Binding { keys: "e", label: "edit", rank: 1, ctx: Ctx::Board },
    Binding { keys: "o", label: "open", rank: 2, ctx: Ctx::Board },
    Binding { keys: "Enter", label: "detail", rank: 2, ctx: Ctx::Board },
    Binding { keys: "H/L", label: "move col", rank: 1, ctx: Ctx::Board },
    Binding { keys: "J/K", label: "reorder", rank: 2, ctx: Ctx::Board },
    Binding { keys: "m", label: "column", rank: 2, ctx: Ctx::Board },
    Binding { keys: "S", label: "status", rank: 2, ctx: Ctx::Board },
    Binding { keys: "p", label: "priority", rank: 2, ctx: Ctx::Board },
    Binding { keys: "t", label: "tags", rank: 2, ctx: Ctx::Board },
    Binding { keys: "dd", label: "delete", rank: 1, ctx: Ctx::Board },
    Binding { keys: "c", label: "note", rank: 1, ctx: Ctx::Board },
    // --- Notes pane ----------------------------------------------------
    Binding { keys: "a/i", label: "new note", rank: 1, ctx: Ctx::Notes },
    Binding { keys: "e", label: "edit", rank: 1, ctx: Ctx::Notes },
    Binding { keys: "dd", label: "delete", rank: 1, ctx: Ctx::Notes },
    Binding { keys: "c", label: "quick capture", rank: 1, ctx: Ctx::Notes },
    Binding { keys: "gp", label: "promote", rank: 1, ctx: Ctx::Notes },
    // --- Global ----------------------------------------------------------
    Binding { keys: "Tab", label: "pane", rank: 1, ctx: Ctx::Global },
    Binding { keys: "N", label: "notes", rank: 1, ctx: Ctx::Global },
    Binding { keys: "b", label: "boards", rank: 2, ctx: Ctx::Global },
    Binding { keys: "u", label: "undo", rank: 2, ctx: Ctx::Global },
    Binding { keys: "^r", label: "redo", rank: 3, ctx: Ctx::Global },
    Binding { keys: "/", label: "search", rank: 1, ctx: Ctx::Global },
    Binding { keys: "n/N", label: "match", rank: 2, ctx: Ctx::Global },
    Binding { keys: ":", label: "cmd", rank: 2, ctx: Ctx::Global },
    Binding { keys: "?", label: "help", rank: 3, ctx: Ctx::Global },
    Binding { keys: "ZZ", label: "quit", rank: 0, ctx: Ctx::Global },
    // --- Help screen -----------------------------------------------------
    Binding { keys: "any", label: "close", rank: 0, ctx: Ctx::Help },
    // --- Form popup --------------------------------------------------------
    Binding { keys: "Tab/S-Tab", label: "field", rank: 0, ctx: Ctx::Form },
    Binding { keys: "^s", label: "save", rank: 0, ctx: Ctx::Form },
    Binding { keys: "Enter", label: "pick/edit", rank: 1, ctx: Ctx::Form },
    Binding { keys: "Esc", label: "cancel", rank: 1, ctx: Ctx::Form },
    // --- Dropdown popup ------------------------------------------------
    Binding { keys: "j/k", label: "move", rank: 0, ctx: Ctx::Dropdown },
    Binding { keys: "gg/G", label: "top/bottom", rank: 2, ctx: Ctx::Dropdown },
    Binding { keys: "Enter", label: "select", rank: 0, ctx: Ctx::Dropdown },
    Binding { keys: "Esc", label: "close", rank: 1, ctx: Ctx::Dropdown },
    // --- Confirm popup -----------------------------------------------------
    Binding { keys: "y/Enter", label: "yes", rank: 0, ctx: Ctx::Confirm },
    Binding { keys: "n/Esc", label: "no", rank: 0, ctx: Ctx::Confirm },
    // --- Passphrase popup ----------------------------------------------
    Binding { keys: "Enter", label: "unlock", rank: 0, ctx: Ctx::Passphrase },
    Binding { keys: "Esc", label: "cancel", rank: 1, ctx: Ctx::Passphrase },

    // --- One-line text prompt (new/rename tag, board name) --------------
    Binding { keys: "Enter", label: "save", rank: 0, ctx: Ctx::TextPrompt },
    Binding { keys: "Esc", label: "cancel", rank: 1, ctx: Ctx::TextPrompt },

    // --- Tag picker -----------------------------------------------------
    Binding { keys: "Enter", label: "toggle tag", rank: 0, ctx: Ctx::TagPicker },
    Binding { keys: "j/k", label: "move", rank: 1, ctx: Ctx::TagPicker },
    Binding { keys: "r", label: "rename", rank: 2, ctx: Ctx::TagPicker },
    Binding { keys: "d", label: "delete", rank: 3, ctx: Ctx::TagPicker },
    Binding { keys: "Esc", label: "close", rank: 4, ctx: Ctx::TagPicker },
];

/// Hints for the footer restricted to exactly the given context, sorted by
/// rank ascending (most important first). Callers combine this with
/// `hints_for(Ctx::Global)` when they want context + global hints together.
pub fn hints_for(ctx: Ctx) -> Vec<&'static Binding> {
    let mut hints: Vec<&'static Binding> = BINDINGS.iter().filter(|b| b.ctx == ctx).collect();
    hints.sort_by_key(|b| b.rank);
    hints
}

/// Resolves a key press to an `Action`. `pending` accumulates counts and
/// multi-key prefixes (`gg`, `dd`, `ZZ`, `gp`) across calls.
///
/// `search_active` disambiguates `N`: it is `ToggleNotesPane` normally, but
/// becomes `SearchPrev` once a search pattern is active. This flag is not
/// part of `Mode`/`Pane`, so it is threaded through explicitly; there is no
/// other way for this function to know it given its inputs.
///
/// Command-mode and Search-mode text entry (ordinary characters, Backspace,
/// Enter to commit) is handled directly by `App` before this is called;
/// this function only resolves Normal-mode (and Insert-mode, unused in
/// Phase 4) key presses. Popup key handling similarly happens in `App`
/// before this is reached: `resolve` is only ever called with an empty
/// popup stack.
pub fn resolve(
    mode: Mode,
    _pane: Pane,
    search_active: bool,
    key: KeyEvent,
    pending: &mut PendingSeq,
) -> Action {
    if !matches!(mode, Mode::Normal | Mode::Insert) {
        return Action::Nop;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // Esc / Ctrl-c always cancel, regardless of any pending prefix or count.
    if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
        pending.clear();
        return Action::Cancel;
    }

    if let Some(prefix) = pending.prefix {
        let action = match (prefix, key.code) {
            ('g', KeyCode::Char('g')) => Action::MoveTop,
            ('g', KeyCode::Char('p')) => Action::PromoteNote,
            ('d', KeyCode::Char('d')) => Action::DeleteSelected,
            ('Z', KeyCode::Char('Z')) => Action::Quit,
            (' ', KeyCode::Char('b')) => Action::OpenBoardSwitcher,
            _ => Action::Nop,
        };
        pending.clear();
        return action;
    }

    if let KeyCode::Char(c) = key.code {
        if !ctrl && c.is_ascii_digit() {
            let d = c.to_digit(10).expect("ascii digit");
            if d != 0 || pending.count.is_some() {
                pending.push_digit(d);
                return Action::Nop;
            }
            // A bare leading '0' is reserved, not a count; fall through
            // to the general table below, where it is unmapped.
        }
    }

    match (ctrl, key.code) {
        (true, KeyCode::Char('d')) => Action::HalfPageDown,
        (true, KeyCode::Char('u')) => Action::HalfPageUp,
        (true, KeyCode::Char('r')) => Action::Redo,
        (false, KeyCode::Char('h')) => Action::FocusPrevColumn,
        (false, KeyCode::Char('l')) => Action::FocusNextColumn,
        (false, KeyCode::Char('j')) => Action::MoveDown(pending.take_count(1)),
        (false, KeyCode::Char('k')) => Action::MoveUp(pending.take_count(1)),
        (false, KeyCode::Char('g')) => {
            pending.set_prefix('g');
            Action::Nop
        }
        (false, KeyCode::Char('d')) => {
            pending.set_prefix('d');
            Action::Nop
        }
        (false, KeyCode::Char('Z')) => {
            pending.set_prefix('Z');
            Action::Nop
        }
        (false, KeyCode::Char(' ')) => {
            pending.set_prefix(' ');
            Action::Nop
        }
        (false, KeyCode::Char('G')) => Action::MoveBottom,
        (false, KeyCode::Tab) => Action::TogglePane,
        (false, KeyCode::Char('a')) | (false, KeyCode::Char('i')) => Action::NewTask,
        (false, KeyCode::Char('e')) => Action::EditSelected,
        (false, KeyCode::Char('o')) => Action::OpenInEditor,
        (false, KeyCode::Enter) => Action::EditSelected,
        (false, KeyCode::Char('H')) => Action::MoveTaskPrevColumn,
        (false, KeyCode::Char('L')) => Action::MoveTaskNextColumn,
        (false, KeyCode::Char('J')) => Action::ReorderTaskDown,
        (false, KeyCode::Char('K')) => Action::ReorderTaskUp,
        (false, KeyCode::Char('m')) => Action::OpenColumnDropdown,
        (false, KeyCode::Char('S')) => Action::OpenColumnDropdown,
        (false, KeyCode::Char('p')) => Action::OpenPriorityDropdown,
        (false, KeyCode::Char('t')) => Action::OpenTagDropdown,
        (false, KeyCode::Char('c')) => Action::CaptureNote,
        (false, KeyCode::Char('N')) => {
            if search_active {
                Action::SearchPrev
            } else {
                Action::ToggleNotesPane
            }
        }
        (false, KeyCode::Char('n')) => Action::SearchNext,
        (false, KeyCode::Char('b')) => Action::OpenBoardSwitcher,
        (false, KeyCode::Char('u')) => Action::Undo,
        (false, KeyCode::Char('/')) => Action::StartSearch,
        (false, KeyCode::Char(':')) => Action::StartCommand,
        (false, KeyCode::Char('?')) => Action::Help,
        _ => Action::Nop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn resolve_n(mode: Mode, search_active: bool, code: KeyCode, pending: &mut PendingSeq) -> Action {
        resolve(mode, Pane::Board, search_active, key(code), pending)
    }

    #[test]
    fn simple_navigation_keys() {
        let mut p = PendingSeq::default();
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('h'), &mut p), Action::FocusPrevColumn);
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('l'), &mut p), Action::FocusNextColumn);
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('j'), &mut p), Action::MoveDown(1));
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('k'), &mut p), Action::MoveUp(1));
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('G'), &mut p), Action::MoveBottom);
    }

    #[test]
    fn count_accumulates_then_applies_to_move() {
        let mut p = PendingSeq::default();
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('1'), &mut p), Action::Nop);
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('0'), &mut p), Action::Nop);
        assert_eq!(p.count, Some(10));
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('j'), &mut p), Action::MoveDown(10));
        assert_eq!(p.count, None);
    }

    #[test]
    fn leading_zero_is_unmapped_not_a_count() {
        let mut p = PendingSeq::default();
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('0'), &mut p), Action::Nop);
        assert_eq!(p.count, None);
    }

    #[test]
    fn gg_sequence_moves_top() {
        let mut p = PendingSeq::default();
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('g'), &mut p), Action::Nop);
        assert_eq!(p.prefix, Some('g'));
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('g'), &mut p), Action::MoveTop);
        assert_eq!(p.prefix, None);
    }

    #[test]
    fn gp_sequence_promotes_note() {
        let mut p = PendingSeq::default();
        resolve_n(Mode::Normal, false, KeyCode::Char('g'), &mut p);
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('p'), &mut p), Action::PromoteNote);
    }

    #[test]
    fn dd_sequence_deletes() {
        let mut p = PendingSeq::default();
        resolve_n(Mode::Normal, false, KeyCode::Char('d'), &mut p);
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('d'), &mut p), Action::DeleteSelected);
    }

    #[test]
    fn zz_sequence_quits() {
        let mut p = PendingSeq::default();
        resolve_n(Mode::Normal, false, KeyCode::Char('Z'), &mut p);
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('Z'), &mut p), Action::Quit);
    }

    #[test]
    fn space_b_sequence_opens_board_switcher() {
        let mut p = PendingSeq::default();
        resolve_n(Mode::Normal, false, KeyCode::Char(' '), &mut p);
        assert_eq!(p.prefix, Some(' '));
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('b'), &mut p), Action::OpenBoardSwitcher);
        assert_eq!(p.prefix, None);
    }

    #[test]
    fn prefix_then_unmapped_key_clears_and_does_nothing() {
        let mut p = PendingSeq::default();
        resolve_n(Mode::Normal, false, KeyCode::Char('g'), &mut p);
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Char('x'), &mut p), Action::Nop);
        assert_eq!(p.prefix, None);
    }

    #[test]
    fn n_disambiguates_on_search_active() {
        let mut p = PendingSeq::default();
        assert_eq!(
            resolve_n(Mode::Normal, false, KeyCode::Char('N'), &mut p),
            Action::ToggleNotesPane
        );
        assert_eq!(
            resolve_n(Mode::Normal, true, KeyCode::Char('N'), &mut p),
            Action::SearchPrev
        );
    }

    #[test]
    fn ctrl_d_and_ctrl_u_are_half_page() {
        let mut p = PendingSeq::default();
        assert_eq!(
            resolve(Mode::Normal, Pane::Board, false, ctrl_key('d'), &mut p),
            Action::HalfPageDown
        );
        assert_eq!(
            resolve(Mode::Normal, Pane::Board, false, ctrl_key('u'), &mut p),
            Action::HalfPageUp
        );
    }

    #[test]
    fn ctrl_c_cancels_and_clears_pending() {
        let mut p = PendingSeq::default();
        resolve_n(Mode::Normal, false, KeyCode::Char('g'), &mut p);
        assert_eq!(
            resolve(Mode::Normal, Pane::Board, false, ctrl_key('c'), &mut p),
            Action::Cancel
        );
        assert_eq!(p.prefix, None);
    }

    #[test]
    fn esc_cancels() {
        let mut p = PendingSeq::default();
        assert_eq!(resolve_n(Mode::Normal, false, KeyCode::Esc, &mut p), Action::Cancel);
    }

    #[test]
    fn hints_for_filters_and_sorts_by_rank() {
        let board_hints = hints_for(Ctx::Board);
        assert!(board_hints.iter().all(|b| b.ctx == Ctx::Board));
        for pair in board_hints.windows(2) {
            assert!(pair[0].rank <= pair[1].rank);
        }
        assert!(!hints_for(Ctx::Global).is_empty());
        assert!(!hints_for(Ctx::Notes).is_empty());
        assert!(!hints_for(Ctx::Help).is_empty());
    }

    #[test]
    fn hints_for_new_popup_contexts_are_nonempty() {
        assert!(!hints_for(Ctx::Form).is_empty());
        assert!(!hints_for(Ctx::Dropdown).is_empty());
        assert!(!hints_for(Ctx::Confirm).is_empty());
        assert!(!hints_for(Ctx::Passphrase).is_empty());
        assert!(!hints_for(Ctx::TextPrompt).is_empty());
        assert!(!hints_for(Ctx::TagPicker).is_empty());
        // A text prompt must not advertise the passphrase popup's "unlock".
        assert!(hints_for(Ctx::TextPrompt).iter().all(|b| b.label != "unlock"));
        // The tag picker must advertise rename and delete, which a plain
        // dropdown has no equivalent of.
        assert!(hints_for(Ctx::TagPicker).iter().any(|b| b.label == "rename"));
        assert!(hints_for(Ctx::TagPicker).iter().any(|b| b.label == "delete"));
    }

    #[test]
    fn shift_s_opens_the_same_status_change_as_m() {
        let mut p = PendingSeq::default();
        assert_eq!(
            resolve_n(Mode::Normal, false, KeyCode::Char('S'), &mut p),
            Action::OpenColumnDropdown
        );
    }

    #[test]
    fn notes_hint_bar_lists_add_edit_delete_capture_promote() {
        let hints = hints_for(Ctx::Notes);
        let labels: Vec<&str> = hints.iter().map(|b| b.label).collect();
        assert!(labels.contains(&"new note"), "{labels:?}");
        assert!(labels.contains(&"edit"), "{labels:?}");
        assert!(labels.contains(&"delete"), "{labels:?}");
        assert!(labels.contains(&"quick capture"), "{labels:?}");
        assert!(labels.contains(&"promote"), "{labels:?}");
    }
}
