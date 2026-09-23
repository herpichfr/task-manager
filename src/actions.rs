//! User-facing actions the keymap resolves keys to, and that `App::dispatch`
//! executes. Variants needed only by a later phase are still accepted by the
//! dispatcher; it reports them as not-yet-implemented rather than panicking
//! or silently dropping them.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Help,
    Refresh,
    Nop,
    /// Cancels the current input (leaves Command/Search mode, clears
    /// buffers and any pending key sequence). Not part of the phase's
    /// originally enumerated action list; added because `Esc`/`Ctrl-c`
    /// cancel is a hard requirement of the keymap and has no other action
    /// to resolve to.
    Cancel,

    MoveUp(u32),
    MoveDown(u32),
    MoveTop,
    MoveBottom,
    HalfPageUp,
    HalfPageDown,

    FocusPrevColumn,
    FocusNextColumn,
    ToggleNotesPane,
    TogglePane,

    MoveTaskPrevColumn,
    MoveTaskNextColumn,
    ReorderTaskUp,
    ReorderTaskDown,

    NewTask,
    EditSelected,
    OpenInEditor,
    DeleteSelected,
    CaptureNote,
    PromoteNote,

    OpenColumnDropdown,
    OpenPriorityDropdown,
    OpenTagDropdown,
    OpenBoardSwitcher,
    OpenArchiveBrowser,

    Undo,
    Redo,
    StartSearch,
    SearchNext,
    SearchPrev,
    StartCommand,
}
