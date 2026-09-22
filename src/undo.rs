//! Undo/redo command log. `App` applies every command's inverse via
//! `TaskStore`; this module only tracks the two stacks and their cap.

use crate::domain::note::{Note, NoteId};
use crate::domain::task::{Status, Task, TaskId, TaskPatch};

/// One reversible action, recorded after it is applied so it can later be
/// undone (and, after an undo, redone).
#[derive(Debug, Clone)]
pub enum Command {
    CreateTask { id: TaskId, status: Status },
    DeleteTask { snapshot: Task, tags: Vec<String> },
    UpdateTask { id: TaskId, before: TaskPatch, after: TaskPatch },
    MoveTask { id: TaskId, from: (Status, i64), to: (Status, i64) },
    CreateNote { id: NoteId },
    DeleteNote { snapshot: Note },
    UpdateNote { id: NoteId, before: String, after: String },
}

/// Maximum number of commands kept on the undo stack. The oldest entry is
/// dropped once the cap is exceeded.
const CAP: usize = 100;

/// Undo/redo command log. `push` records a newly-applied command and
/// clears the redo stack (a fresh action invalidates whatever could have
/// been redone). `undo`/`redo` only pop; `App` computes each command's
/// inverse against `TaskStore` and uses `push_undo`/`push_redo` to move the
/// resulting (possibly id-updated) command onto the other stack, so that
/// re-creating an undone delete under a new row id keeps both stacks
/// consistent without either side needing to know about that id change.
#[derive(Debug, Default)]
pub struct UndoStack {
    undo: Vec<Command>,
    redo: Vec<Command>,
}

impl UndoStack {
    /// Records a newly-applied command and clears the redo stack.
    pub fn push(&mut self, c: Command) {
        Self::push_capped(&mut self.undo, c);
        self.redo.clear();
    }

    /// Pops the most recent undoable command, if any. The caller applies
    /// its inverse and, on success, pushes the resulting command onto the
    /// redo side via `push_redo`.
    pub fn undo(&mut self) -> Option<Command> {
        self.undo.pop()
    }

    /// Pops the most recent redoable command, if any. The caller re-applies
    /// it and, on success, pushes the resulting command back onto the undo
    /// side via `push_undo`.
    pub fn redo(&mut self) -> Option<Command> {
        self.redo.pop()
    }

    /// Discards everything on the redo stack.
    pub fn clear_redo(&mut self) {
        self.redo.clear();
    }

    /// Moves a command onto the redo stack without touching the undo
    /// stack or clearing anything. Used only while undoing.
    pub fn push_redo(&mut self, c: Command) {
        Self::push_capped(&mut self.redo, c);
    }

    /// Moves a command onto the undo stack without touching the redo
    /// stack or clearing anything. Used only while redoing.
    pub fn push_undo(&mut self, c: Command) {
        Self::push_capped(&mut self.undo, c);
    }

    fn push_capped(stack: &mut Vec<Command>, c: Command) {
        stack.push(c);
        if stack.len() > CAP {
            stack.remove(0);
        }
    }

    #[cfg(test)]
    pub(crate) fn undo_len(&self) -> usize {
        self.undo.len()
    }

    #[cfg(test)]
    pub(crate) fn redo_len(&self) -> usize {
        self.redo.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(id: TaskId) -> Command {
        Command::CreateTask { id, status: Status::ToDo }
    }

    #[test]
    fn push_then_undo_pops_last() {
        let mut s = UndoStack::default();
        s.push(cmd(1));
        s.push(cmd(2));
        match s.undo() {
            Some(Command::CreateTask { id, .. }) => assert_eq!(id, 2),
            _ => panic!("expected CreateTask"),
        }
    }

    #[test]
    fn undo_then_push_redo_then_redo_round_trips() {
        let mut s = UndoStack::default();
        s.push(cmd(1));
        let popped = s.undo().unwrap();
        s.push_redo(popped);
        assert_eq!(s.redo_len(), 1);
        match s.redo() {
            Some(Command::CreateTask { id, .. }) => assert_eq!(id, 1),
            _ => panic!("expected CreateTask"),
        }
    }

    #[test]
    fn push_clears_redo_stack() {
        let mut s = UndoStack::default();
        s.push(cmd(1));
        let popped = s.undo().unwrap();
        s.push_redo(popped);
        assert_eq!(s.redo_len(), 1);
        s.push(cmd(2));
        assert_eq!(s.redo_len(), 0);
    }

    #[test]
    fn clear_redo_empties_it() {
        let mut s = UndoStack::default();
        s.push(cmd(1));
        let popped = s.undo().unwrap();
        s.push_redo(popped);
        s.clear_redo();
        assert_eq!(s.redo_len(), 0);
    }

    #[test]
    fn undo_on_empty_stack_is_none() {
        let mut s = UndoStack::default();
        assert!(s.undo().is_none());
        assert!(s.redo().is_none());
    }

    #[test]
    fn cap_drops_oldest_entry() {
        let mut s = UndoStack::default();
        for i in 0..150 {
            s.push(cmd(i));
        }
        assert_eq!(s.undo_len(), 100);
        // The oldest 50 pushes (ids 0..50) should have been dropped; the
        // next pop must be the most recent (id 149).
        match s.undo() {
            Some(Command::CreateTask { id, .. }) => assert_eq!(id, 149),
            _ => panic!("expected CreateTask"),
        }
    }

    #[test]
    fn push_undo_does_not_touch_redo() {
        let mut s = UndoStack::default();
        s.push(cmd(1));
        let popped = s.undo().unwrap();
        s.push_redo(popped);
        let redone = s.redo().unwrap();
        s.push_undo(redone);
        assert_eq!(s.undo_len(), 1);
        assert_eq!(s.redo_len(), 0);
    }
}
