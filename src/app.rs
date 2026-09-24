//! Application state: the model the TUI renders and the key dispatcher
//! mutates.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::actions::Action;
use crate::config::{self, Config};
use crate::domain::board::{Board, BoardId, BoardKind};
use crate::domain::note::{Note, NoteId};
use crate::domain::task::{NewTask, Priority, Status, Tag, TagId, Task, TaskId, TaskPatch};
use crate::error::{AppError, Result};
use crate::keymap::{self, Ctx};
use crate::mode::{Mode, PendingSeq};
use crate::storage::locked_db::LockedDb;
use crate::storage::main_db::MainDb;
use crate::storage::{StorageError, TaskStore};
use crate::ui::forms::{Field, FormKind, FormState, TaskDraft};
use crate::ui::popup::{
    ArchiveBrowserState, ConfirmState, DropdownState, DropdownTarget, FormTagPickerAction, FormTagPickerState,
    PassphraseState, Popup, PopupOutcome, PopupValue, SelectItem, TagPickerAction, TagPickerState, TextPromptState,
};
use crate::ui::theme::{self, Styles};
use crate::undo::{Command, UndoStack};

/// Which pane currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Board,
    Notes,
}

/// A single kanban column's cached tasks and cursor position.
#[derive(Debug, Clone, Default)]
pub struct ColumnState {
    pub tasks: Vec<Task>,
    pub selected: usize,
}

/// What a still-open (non-form) popup on top of an otherwise-empty stack
/// should do with the value it eventually submits. A form-nested dropdown
/// (opened from the Status/Priority/Tags field of an open task form) never
/// needs this: it submits into the form via `Popup::receive` instead,
/// since the form is still on the stack underneath it.
#[derive(Debug, Clone)]
enum PendingPopupAction {
    MoveTaskColumn(TaskId),
    SetPriority(TaskId),
    PromoteNote(NoteId),
    ConfirmDeleteTask(TaskId),
    ConfirmDeleteNote(NoteId),
    /// A new-tag text prompt is open for this task: the typed name is
    /// created (or reused, if it already exists) and applied to the task.
    NewTagForTask(TaskId),
    /// A rename-tag text prompt is open for this tag id.
    RenameTag(TagId),
    /// The delete-tag confirmation is open for this tag id.
    ConfirmDeleteTag(TagId),
    /// The one-line quick-capture prompt (`c` on the board) is open.
    QuickCaptureNote,
    /// A dropdown of boards is open; the selected item's id is the target
    /// board id, so this variant itself carries no payload.
    SwitchBoard,
    /// A passphrase prompt is open to unlock this board id.
    UnlockBoard(BoardId),
    /// First passphrase entry for a new locked board named `name`.
    NewLockedBoardPass1 { name: String },
    /// Second (confirmation) passphrase entry; `first` is what was typed
    /// the first time, to compare against.
    NewLockedBoardPass2 { name: String, first: String },
    /// The unrecoverable-data warning is open; creates the board on yes.
    ConfirmCreateLockedBoard { name: String, passphrase: String },
    /// The delete-board confirmation is open for this board id.
    ConfirmDeleteBoard(BoardId),
    /// A one-line prompt for a new plain board's name is open (the board
    /// switcher's `a`, or the `:board` menu's "New board").
    NewBoardPlain,
    /// A one-line prompt for a new locked board's name is open (the board
    /// switcher's `A`, or the `:board` menu's "New locked board"); the
    /// passphrase entry that follows reuses `NewLockedBoardPass1`.
    NewBoardLocked,
    /// A one-line prompt to rename the *current* board is open (the
    /// `:board` menu's "Rename current board").
    RenameCurrentBoard,
    /// The bare `:board` menu is open.
    BoardMenu,
    /// A dropdown listing every board is open so the user can pick which
    /// one to delete (the `:board` menu's "Delete board…").
    SelectBoardToDelete,
}

pub struct App {
    pub mode: Mode,
    pub pending: PendingSeq,
    pub pane: Pane,
    pub show_notes: bool,
    pub cmdline: String,
    pub search: String,
    pub search_active: bool,
    pub message: Option<String>,
    pub columns: [ColumnState; 3],
    pub notes: Vec<Note>,
    pub notes_selected: usize,
    pub focused: usize,
    pub board: Board,
    pub config: Config,
    pub should_quit: bool,
    pub styles: Styles,
    pub db: MainDb,
    /// The popup stack: empty when no dropdown/form/confirm/passphrase/help
    /// is open. Rendered bottom to top; keys go to the top entry only.
    pub popups: Vec<Popup>,
    undo: UndoStack,
    pending_action: Option<PendingPopupAction>,
    /// Set when a key press asked for the external editor. `App` never runs
    /// the editor itself: it only records the request, and the event loop --
    /// which owns the one real `Terminal` -- performs it and calls
    /// [`App::apply_edited_text`]. Building a second `Terminal` here instead
    /// made `terminal.clear()` fail with "the cursor position could not be
    /// read", which silently discarded whatever the user had just written.
    pending_edit: Option<(EditTarget, String)>,
    /// Locked boards' open connections, kept only for the lifetime of this
    /// process and never persisted. An entry exists exactly while that
    /// board is unlocked; `:lock` always removes the current board's entry,
    /// and switching away from a locked board removes it too when
    /// `config.lock_on_board_switch` is set (the default).
    unlocked: HashMap<BoardId, LockedDb>,
    /// Directory a newly created locked board's encrypted file is written
    /// into. Defaults to the current directory; `main.rs` overrides it via
    /// [`App::set_data_dir`] with the same resolved data directory
    /// `main.db` lives in, once at startup.
    data_dir: PathBuf,
    /// True right after a lone `d` is pressed on the board switcher's
    /// `Board`-target dropdown, so a second `d` (completing `dd`) deletes
    /// the highlighted board. Reset on any other key and whenever the
    /// switcher is (re)opened, so it never survives past the dropdown it
    /// was set on.
    board_switcher_pending_d: bool,
    /// Snapshot of `main.db`'s on-disk identity, refreshed at the end of
    /// every `reload()`. `None` for an in-memory connection (every test
    /// that does not open a real file) or before the first reload.
    fp_main: Option<FileFingerprint>,
    /// Same, for the active locked board's own file -- only ever `Some`
    /// while `self.board` is a locked board that is currently unlocked.
    fp_locked: Option<FileFingerprint>,
    /// Set by every `check_external_change` call; rate-limits the event
    /// loop's ~250ms tick to about once a second. `None` forces the next
    /// call to actually check, which is also how `Action::Refresh` reaches
    /// `reconcile_external_change` directly, bypassing the limit.
    last_external_check: Option<std::time::Instant>,
}

/// What an external-editor session is editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTarget {
    /// A task's markdown body, saved straight to storage on return.
    Task(TaskId),
    /// The Body field of the form currently on top of the popup stack.
    FormBody,
}

/// How many rows a half-page scroll moves, absent a known viewport height.
const HALF_PAGE: u32 = 5;

/// Minimum spacing between the on-disk-change checks the event loop's
/// ~250ms tick drives (`App::check_external_change`), so an idle session
/// doesn't `stat` up to two files four times a second for nothing. An
/// explicit `:e`/`:reload`/`Action::Refresh` bypasses this by calling
/// `reconcile_external_change` directly.
const EXTERNAL_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// A cheap, comparable snapshot of a database file's identity and content
/// on disk: device + inode (so a replace-by-rename -- e.g. Dropbox
/// swapping in the other machine's synced copy -- is detected even if
/// length and mtime happen to match) plus length and mtime (so an
/// in-place rewrite that keeps the same inode is still caught).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
}

/// `None` when the path cannot be `stat`-ed -- missing, or mid-replace by a
/// sync tool -- which callers treat as "no known change" rather than an
/// error, so a transient gap never mistakenly looks like a match *or* a
/// mismatch.
fn file_fingerprint(path: &std::path::Path) -> Option<FileFingerprint> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(FileFingerprint { dev: meta.dev(), ino: meta.ino(), len: meta.len(), mtime: meta.mtime() })
    }
    #[cfg(not(unix))]
    {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Some(FileFingerprint { dev: 0, ino: 0, len: meta.len(), mtime })
    }
}

/// Days after which a Done task is auto-archived: hidden from the board
/// (and so from the board's `/` search, which only ever sees
/// `self.columns`) but kept in storage, browsable via `A`. See
/// `is_archived`.
const ARCHIVE_AFTER_DAYS: i64 = 5;
const ARCHIVE_AFTER_SECS: i64 = ARCHIVE_AFTER_DAYS * 86_400;

/// Maps storage-layer errors onto the crate's shared `AppError`. `error.rs`
/// has no variant of its own for `StorageError`, so this is a plain
/// conversion function rather than a `From` impl.
pub fn storage_err(e: StorageError) -> AppError {
    match e {
        StorageError::Sqlite(err) => AppError::Sqlite(err),
        StorageError::Io(err) => AppError::Io(err),
        StorageError::NotFound => AppError::Config("not found".to_string()),
        StorageError::BoardExists(name) => {
            AppError::Config(format!("board already exists: {name}"))
        }
        StorageError::InvalidEnum(v) => AppError::Config(format!("invalid enum value: {v}")),
        StorageError::WrongPassphrase => AppError::WrongPassphrase,
        StorageError::EmptyTagName => AppError::Config("tag name cannot be empty".to_string()),
    }
}

/// Case-insensitive "does this task match a search query" test, shared by
/// live filtering (`ui::board`) and `n`/`N` match-jumping (`App::jump_search`).
/// A task matches if its title, body, or any tag name contains `query`.
pub(crate) fn task_matches_query(task: &Task, query: &str) -> bool {
    let q = query.to_lowercase();
    task.title.to_lowercase().contains(&q)
        || task.body.to_lowercase().contains(&q)
        || task.tags.iter().any(|t| t.name.to_lowercase().contains(&q))
}

/// Case-insensitive "does this note match a search query" test: matches on
/// the note's body.
pub(crate) fn note_matches_query(note: &Note, query: &str) -> bool {
    note.body.to_lowercase().contains(&query.to_lowercase())
}

/// First-run bootstrap and board selection.
///
/// If the database has no boards yet, creates one named from
/// `default_board` (or `"personal"`). Selection order: `requested` (the
/// `--board` flag) if given -- and an unmatched name is an error, never a
/// silent fallback; else `default_board` if it names an existing board;
/// else the first board by position.
pub fn bootstrap_and_select_board(
    db: &MainDb,
    requested: Option<&str>,
    default_board: Option<&str>,
) -> Result<Board> {
    let boards = db.list_boards().map_err(storage_err)?;
    if boards.is_empty() {
        let name = default_board.unwrap_or("personal");
        db.create_board(name, BoardKind::Plain).map_err(storage_err)?;
    }

    let mut boards = db.list_boards().map_err(storage_err)?;

    if let Some(name) = requested {
        return boards
            .into_iter()
            .find(|b| b.name == name)
            .ok_or_else(|| AppError::Config(format!("no such board: {name}")));
    }

    if let Some(name) = default_board {
        if let Some(pos) = boards.iter().position(|b| b.name == name) {
            return Ok(boards.remove(pos));
        }
    }

    boards
        .into_iter()
        .next()
        .ok_or_else(|| AppError::Config("no boards available".to_string()))
}

/// Refuses to start the app directly on a locked board: unlocking needs an
/// interactive passphrase prompt, which only exists once the TUI's popup
/// stack is running. Start on a plain board and switch to a locked one
/// with `b`/`:board` from inside a running session instead.
pub fn ensure_startable(board: &Board) -> Result<()> {
    if board.kind == BoardKind::Locked {
        return Err(AppError::Config(format!(
            "cannot start directly on locked board \"{}\"; start on a plain board and switch to it from inside tsk",
            board.name
        )));
    }
    Ok(())
}

fn status_label(status: Status) -> &'static str {
    match status {
        Status::ToDo => "ToDo",
        Status::Doing => "Doing",
        Status::Done => "Done",
    }
}

fn combine_note_body(title: &str, body: &str) -> String {
    if body.trim().is_empty() {
        title.to_string()
    } else {
        format!("{title}\n{body}")
    }
}

fn column_dropdown_items() -> Vec<SelectItem> {
    Status::ALL
        .iter()
        .enumerate()
        .map(|(i, s)| SelectItem { id: i as i64, label: status_label(*s).to_string() })
        .collect()
}

fn priority_dropdown_items(current: Priority) -> (Vec<SelectItem>, usize) {
    let items = Priority::ALL
        .iter()
        .map(|p| SelectItem { id: *p as i64, label: p.as_str().to_string() })
        .collect();
    let selected = Priority::ALL.iter().position(|p| *p == current).unwrap_or(0);
    (items, selected)
}

/// Finds `current` in `matches` (a sorted list of raw indices that satisfy
/// a search query) and returns the next/previous entry, wrapping around;
/// `dir >= 0` is "next", negative is "previous". When `current` is not
/// itself a match (e.g. selection sits on a filtered-out row), jumps to
/// the first match in the requested direction instead of wrapping from an
/// undefined position. Never called with an empty `matches`.
fn advance_in(matches: &[usize], current: usize, dir: i32) -> usize {
    let len = matches.len() as i32;
    let pos = matches.iter().position(|&m| m == current);
    let new_pos = match pos {
        Some(p) => (p as i32 + dir).rem_euclid(len),
        None => {
            if dir >= 0 {
                0
            } else {
                len - 1
            }
        }
    };
    matches[new_pos as usize]
}

/// Steps `delta` positions through `visible` (ascending raw indices) from
/// wherever `current` sits among them, clamped to the ends -- plain
/// movement (`j`/`k`, `gg`/`G`, `^d`/`^u`) never wraps, unlike `advance_in`'s
/// `n`/`N` jump. When `current` is not itself in `visible` (the search
/// filter just hid it), lands on the first visible entry in the direction
/// of travel -- the same "current not a match" convention `advance_in`
/// uses. `None` when nothing is visible, so callers make no change rather
/// than landing on a hidden row.
fn step_visible(visible: &[usize], current: usize, delta: i64) -> Option<usize> {
    if visible.is_empty() {
        return None;
    }
    let last = visible.len() as i64 - 1;
    let new_pos = match visible.iter().position(|&i| i == current) {
        Some(p) => (p as i64 + delta).clamp(0, last),
        None if delta >= 0 => 0,
        None => last,
    };
    Some(visible[new_pos as usize])
}

/// Sort key for the ToDo/Doing columns: a task with a deadline sorts by it
/// ascending -- an overdue deadline is a smaller/more negative timestamp,
/// so it naturally sorts first, most overdue first -- and a task with no
/// deadline sorts after every task that has one. `position` tiebreaks
/// equal deadlines, which is also exactly the pair `reorder_selected`'s
/// swap-guard (`same_sort_bucket`) allows `J`/`K` to touch.
fn deadline_sort_key(task: &Task) -> (u8, i64, i64) {
    match task.deadline {
        Some(d) => (0, d, task.position),
        None => (1, 0, task.position),
    }
}

/// Sort key for the Done column: descending `completed_at` (most recent
/// first), `position` tiebreaking equal timestamps. Every Done task is
/// stamped by `TaskStore::move_task`/`create_task`/`promote_note` (see
/// `storage::task_store_impl`), so `None` should not occur here in
/// practice; it sorts last rather than panicking if it ever does.
fn completed_sort_key(task: &Task) -> (u8, Reverse<i64>, i64) {
    match task.completed_at {
        Some(c) => (0, Reverse(c), task.position),
        None => (1, Reverse(0), task.position),
    }
}

/// True once a Done task's `completed_at` is more than `ARCHIVE_AFTER_DAYS`
/// days in the past. A task with no `completed_at` is never archived.
fn is_archived(task: &Task, now: i64) -> bool {
    match task.completed_at {
        Some(c) => now - c > ARCHIVE_AFTER_SECS,
        None => false,
    }
}

/// Whether `a` and `b` sit in the same automatic-sort bucket for column
/// `idx`, i.e. whether `J`/`K` may swap them without producing an order
/// `reload()`'s sort would immediately undo: the Done column compares
/// `completed_at`, ToDo/Doing compare `deadline`.
fn same_sort_bucket(idx: usize, a: &Task, b: &Task) -> bool {
    if Status::ALL[idx] == Status::Done {
        a.completed_at == b.completed_at
    } else {
        a.deadline == b.deadline
    }
}

/// The status message `reorder_selected` reports when `J`/`K` is refused
/// because the two rows do not share a sort bucket.
fn sort_bucket_mismatch_message(idx: usize) -> String {
    if Status::ALL[idx] == Status::Done {
        "Done is sorted by completion time; J/K only swaps tasks completed at the same time".to_string()
    } else {
        "column is sorted by deadline; J/K only swaps tasks with the same deadline".to_string()
    }
}

/// The active board's store: either the shared main database's rows
/// (`PlainBoardStore`) or an unlocked `LockedDb`'s own file
/// (`LockedBoardStore`). See `App::store` for why this is a concrete enum
/// rather than `Box<dyn TaskStore>`.
enum ActiveStore<'a> {
    Plain(crate::storage::main_db::PlainBoardStore<'a>),
    Locked(crate::storage::locked_db::LockedBoardStore<'a>),
}

impl<'a> TaskStore for ActiveStore<'a> {
    fn list_tasks(&self, status: Status) -> std::result::Result<Vec<Task>, StorageError> {
        match self {
            ActiveStore::Plain(s) => s.list_tasks(status),
            ActiveStore::Locked(s) => s.list_tasks(status),
        }
    }
    fn get_task(&self, id: TaskId) -> std::result::Result<Task, StorageError> {
        match self {
            ActiveStore::Plain(s) => s.get_task(id),
            ActiveStore::Locked(s) => s.get_task(id),
        }
    }
    fn create_task(&self, draft: NewTask) -> std::result::Result<TaskId, StorageError> {
        match self {
            ActiveStore::Plain(s) => s.create_task(draft),
            ActiveStore::Locked(s) => s.create_task(draft),
        }
    }
    fn update_task(&self, id: TaskId, patch: TaskPatch) -> std::result::Result<(), StorageError> {
        match self {
            ActiveStore::Plain(s) => s.update_task(id, patch),
            ActiveStore::Locked(s) => s.update_task(id, patch),
        }
    }
    fn mark_deadline_notified(&self, id: TaskId) -> std::result::Result<(), StorageError> {
        match self {
            ActiveStore::Plain(s) => s.mark_deadline_notified(id),
            ActiveStore::Locked(s) => s.mark_deadline_notified(id),
        }
    }
    fn move_task(&self, id: TaskId, to: Status, index: i64) -> std::result::Result<(), StorageError> {
        match self {
            ActiveStore::Plain(s) => s.move_task(id, to, index),
            ActiveStore::Locked(s) => s.move_task(id, to, index),
        }
    }
    fn reorder(&self, status: Status, ordered: &[TaskId]) -> std::result::Result<(), StorageError> {
        match self {
            ActiveStore::Plain(s) => s.reorder(status, ordered),
            ActiveStore::Locked(s) => s.reorder(status, ordered),
        }
    }
    fn delete_task(&self, id: TaskId) -> std::result::Result<(), StorageError> {
        match self {
            ActiveStore::Plain(s) => s.delete_task(id),
            ActiveStore::Locked(s) => s.delete_task(id),
        }
    }
    fn list_notes(&self) -> std::result::Result<Vec<Note>, StorageError> {
        match self {
            ActiveStore::Plain(s) => s.list_notes(),
            ActiveStore::Locked(s) => s.list_notes(),
        }
    }
    fn create_note(&self, body: &str) -> std::result::Result<NoteId, StorageError> {
        match self {
            ActiveStore::Plain(s) => s.create_note(body),
            ActiveStore::Locked(s) => s.create_note(body),
        }
    }
    fn update_note(&self, id: NoteId, body: &str) -> std::result::Result<(), StorageError> {
        match self {
            ActiveStore::Plain(s) => s.update_note(id, body),
            ActiveStore::Locked(s) => s.update_note(id, body),
        }
    }
    fn delete_note(&self, id: NoteId) -> std::result::Result<(), StorageError> {
        match self {
            ActiveStore::Plain(s) => s.delete_note(id),
            ActiveStore::Locked(s) => s.delete_note(id),
        }
    }
    fn promote_note(&self, id: NoteId, status: Status) -> std::result::Result<TaskId, StorageError> {
        match self {
            ActiveStore::Plain(s) => s.promote_note(id, status),
            ActiveStore::Locked(s) => s.promote_note(id, status),
        }
    }
    fn list_tags(&self) -> std::result::Result<Vec<Tag>, StorageError> {
        match self {
            ActiveStore::Plain(s) => s.list_tags(),
            ActiveStore::Locked(s) => s.list_tags(),
        }
    }
    fn upsert_tag(&self, name: &str, color: Option<&str>) -> std::result::Result<TagId, StorageError> {
        match self {
            ActiveStore::Plain(s) => s.upsert_tag(name, color),
            ActiveStore::Locked(s) => s.upsert_tag(name, color),
        }
    }
    fn rename_tag(&self, id: TagId, new_name: &str) -> std::result::Result<(), StorageError> {
        match self {
            ActiveStore::Plain(s) => s.rename_tag(id, new_name),
            ActiveStore::Locked(s) => s.rename_tag(id, new_name),
        }
    }
    fn delete_tag(&self, id: TagId) -> std::result::Result<(), StorageError> {
        match self {
            ActiveStore::Plain(s) => s.delete_tag(id),
            ActiveStore::Locked(s) => s.delete_tag(id),
        }
    }
    fn tags_for_task(&self, id: TaskId) -> std::result::Result<Vec<Tag>, StorageError> {
        match self {
            ActiveStore::Plain(s) => s.tags_for_task(id),
            ActiveStore::Locked(s) => s.tags_for_task(id),
        }
    }
    fn set_task_tags(&self, id: TaskId, tags: &[TagId]) -> std::result::Result<(), StorageError> {
        match self {
            ActiveStore::Plain(s) => s.set_task_tags(id, tags),
            ActiveStore::Locked(s) => s.set_task_tags(id, tags),
        }
    }
}

/// Upserts each name in `names` (creating any that don't already exist)
/// and sets exactly that set on `task_id`. Shared by `apply_task_draft`'s
/// `NewTask` and `EditTask` arms.
fn apply_draft_tags(store: &ActiveStore<'_>, task_id: TaskId, names: &[String]) -> std::result::Result<(), StorageError> {
    let mut ids = Vec::with_capacity(names.len());
    for name in names {
        ids.push(store.upsert_tag(name, None)?);
    }
    store.set_task_tags(task_id, &ids)
}

/// `SelectItem` ids for the bare `:board` menu's rows
/// (`App::open_board_menu` / `App::apply_board_menu_selection`).
const BOARD_MENU_NEW: i64 = 0;
const BOARD_MENU_NEW_LOCKED: i64 = 1;
const BOARD_MENU_RENAME: i64 = 2;
const BOARD_MENU_DELETE: i64 = 3;
const BOARD_MENU_SWITCH: i64 = 4;
const BOARD_MENU_LOCK: i64 = 5;
const BOARD_MENU_INFO: i64 = 6;

impl App {
    /// Builds the app for `board`, loading its tasks and notes from `db`.
    pub fn new(config: Config, db: MainDb, board: Board) -> Result<Self> {
        let depth = theme::detect_depth(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        );
        let styles = theme::resolve(&config.theme, depth);

        let mut app = App {
            mode: Mode::Normal,
            pending: PendingSeq::default(),
            pane: Pane::Board,
            show_notes: false,
            cmdline: String::new(),
            search: String::new(),
            search_active: false,
            message: None,
            columns: [
                ColumnState::default(),
                ColumnState::default(),
                ColumnState::default(),
            ],
            notes: Vec::new(),
            notes_selected: 0,
            focused: 0,
            board,
            config,
            should_quit: false,
            styles,
            db,
            popups: Vec::new(),
            undo: UndoStack::default(),
            pending_action: None,
            pending_edit: None,
            unlocked: HashMap::new(),
            data_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            board_switcher_pending_d: false,
            fp_main: None,
            fp_locked: None,
            last_external_check: None,
        };
        app.reload()?;
        Ok(app)
    }

    /// Sets the directory a newly created locked board's encrypted file is
    /// written into. See the `data_dir` field doc.
    pub fn set_data_dir(&mut self, dir: PathBuf) {
        self.data_dir = dir;
    }

    /// The `TaskStore` for whichever board is currently active: the shared
    /// main database's rows for a plain board, or -- once unlocked -- the
    /// cached `LockedDb` for a locked one. Every existing task/note action
    /// goes through this single method, so nothing else in `App` branches
    /// on board kind.
    ///
    /// Returns the concrete `ActiveStore` enum rather than `Box<dyn
    /// TaskStore>`: a boxed trait object is conservatively assumed by the
    /// borrow checker to run arbitrary code when dropped (it doesn't know
    /// the concrete type has no `Drop` impl), which extends the borrow of
    /// `self` all the way to the box's actual drop point instead of its
    /// last use -- breaking every call site below that reads from `store`
    /// and then mutates a *different* field of `self` (message/undo/
    /// columns) in the same function. `ActiveStore` has no `Drop` impl of
    /// its own, so none of that applies and last-use-based NLL works as it
    /// always did when this returned a concrete `PlainBoardStore`.
    fn store(&self) -> ActiveStore<'_> {
        if self.board.kind == BoardKind::Locked {
            if let Some(locked) = self.unlocked.get(&self.board.id) {
                return ActiveStore::Locked(locked.store());
            }
        }
        ActiveStore::Plain(self.db.store_for(self.board.id))
    }

    /// Re-reads tasks and notes from storage. Each column's (and the notes
    /// list's) cursor is clamped to the reloaded length rather than reset.
    ///
    /// Everything is read from `store()` before anything on `self` is
    /// mutated: `store()` borrows all of `self` (not just `self.db`, since
    /// a locked board's store instead comes from `self.unlocked`), so
    /// interleaving reads and writes to `self.columns` within one loop --
    /// as the equivalent plain-board-only version once did -- would not
    /// borrow-check.
    pub fn reload(&mut self) -> Result<()> {
        let store = self.store();
        let mut all_tasks: Vec<Vec<Task>> = Vec::with_capacity(Status::ALL.len());
        for status in Status::ALL.iter() {
            all_tasks.push(store.list_tasks(*status).map_err(storage_err)?);
        }
        let notes = store.list_notes().map_err(storage_err)?;

        let now = chrono::Utc::now().timestamp();
        for (i, mut tasks) in all_tasks.into_iter().enumerate() {
            if Status::ALL[i] == Status::Done {
                // Archived rows never reach `self.columns` at all, which is
                // what keeps the board's `/` search (which only ever walks
                // `self.columns`) acting on visible tasks alone.
                tasks.retain(|t| !is_archived(t, now));
                tasks.sort_by_key(completed_sort_key);
            } else {
                tasks.sort_by_key(deadline_sort_key);
            }
            let old_selected = self.columns[i].selected;
            let selected = if tasks.is_empty() {
                0
            } else {
                old_selected.min(tasks.len() - 1)
            };
            self.columns[i] = ColumnState { tasks, selected };
        }
        self.notes = notes;
        self.notes_selected = if self.notes.is_empty() {
            0
        } else {
            self.notes_selected.min(self.notes.len() - 1)
        };
        self.clamp_selection_to_visible();
        self.record_fingerprints();
        Ok(())
    }

    /// Captures the current on-disk fingerprint of the active database
    /// file(s), so a later mismatch means something else touched them.
    /// Called at the end of every `reload()` -- which every mutating
    /// action already goes through -- so the app's own writes are never
    /// mistaken for an external change.
    fn record_fingerprints(&mut self) {
        self.fp_main = self.db.path().and_then(file_fingerprint);
        self.fp_locked = if self.board.kind == BoardKind::Locked {
            self.board.db_path.as_deref().and_then(|p| file_fingerprint(std::path::Path::new(p)))
        } else {
            None
        };
    }

    /// Called from the event loop's ~250ms tick. Rate-limited to about
    /// once a second via `last_external_check`; `Action::Refresh` (bound
    /// to `:e`/`:reload`) bypasses the limit by calling
    /// `reconcile_external_change` directly instead.
    pub fn check_external_change(&mut self) {
        let now = std::time::Instant::now();
        if let Some(last) = self.last_external_check {
            if now.duration_since(last) < EXTERNAL_CHECK_INTERVAL {
                return;
            }
        }
        self.last_external_check = Some(now);
        self.reconcile_external_change();
    }

    /// Active tasks whose deadline has passed and has not yet produced a
    /// desktop notification. The event loop calls this every tick; the
    /// notification is persisted only after the desktop service accepts it.
    pub fn unnotified_deadline_tasks(&self) -> Vec<Task> {
        let now = chrono::Utc::now().timestamp();
        self.columns[..2]
            .iter()
            .flat_map(|column| column.tasks.iter())
            .filter(|task| task.deadline.is_some_and(|deadline| deadline <= now) && task.deadline_notified_at.is_none())
            .cloned()
            .collect()
    }

    /// Records that a deadline alert was delivered and updates the cached
    /// task, preventing another event-loop tick from sending it again.
    pub fn mark_deadline_notified(&mut self, id: TaskId) -> Result<()> {
        self.store().mark_deadline_notified(id).map_err(storage_err)?;
        self.record_fingerprints();
        for column in &mut self.columns[..2] {
            if let Some(task) = column.tasks.iter_mut().find(|task| task.id == id) {
                task.deadline_notified_at = Some(chrono::Utc::now().timestamp());
                break;
            }
        }
        Ok(())
    }

    /// Compares the active database file(s) against the last-recorded
    /// fingerprint and, on a mismatch, reopens and reloads. Returns
    /// whether it did. A missing file (mid-replace, or briefly absent) is
    /// treated as "no known change" rather than an error: the old
    /// connection and state are kept, and the next check retries.
    ///
    /// `App` does not retain the passphrase-derived key after unlocking a
    /// board -- only the open `LockedDb` connection in `self.unlocked` --
    /// so a changed locked-board file cannot be reopened here; it is
    /// locked cleanly instead, with a message asking the user to re-enter
    /// the passphrase.
    fn reconcile_external_change(&mut self) -> bool {
        let main_fp = self.db.path().and_then(file_fingerprint);
        let main_changed = main_fp.is_some() && main_fp != self.fp_main;

        let locked_fp = if self.board.kind == BoardKind::Locked {
            self.board.db_path.as_deref().and_then(|p| file_fingerprint(std::path::Path::new(p)))
        } else {
            None
        };
        let locked_changed = locked_fp.is_some() && locked_fp != self.fp_locked;

        if !main_changed && !locked_changed {
            return false;
        }

        if main_changed {
            let Some(main_path) = self.db.path().map(std::path::Path::to_path_buf) else {
                return false;
            };
            match MainDb::open(&main_path, self.config.journal_mode) {
                Ok(db) => self.db = db,
                Err(e) => {
                    self.message = Some(format!(
                        "database changed on disk but reopen failed, keeping the previous connection: {e}"
                    ));
                    return false;
                }
            }
        }

        let mut relocked_name = None;
        if locked_changed {
            self.unlocked.remove(&self.board.id);
            relocked_name = Some(self.board.name.clone());
            let boards = self.db.list_boards().unwrap_or_default();
            if let Some(plain) = boards.into_iter().find(|b| b.kind == BoardKind::Plain) {
                self.board = plain;
                self.pane = Pane::Board;
                self.focused = 0;
            }
        }

        self.undo = UndoStack::default();
        let _ = self.reload();
        self.message = Some(match relocked_name {
            Some(name) => format!(
                "reloaded: database changed on disk; \"{name}\" was locked, re-enter the passphrase to unlock"
            ),
            None => "reloaded: database changed on disk".to_string(),
        });
        true
    }

    /// The footer context this app's current state should show hints for.
    pub fn ctx(&self) -> Ctx {
        if let Some(top) = self.popups.last() {
            return match top {
                Popup::Dropdown(_) => Ctx::Dropdown,
                Popup::Form(_) => Ctx::Form,
                Popup::Confirm(_) => Ctx::Confirm,
                Popup::Passphrase(_) => Ctx::Passphrase,
                Popup::TagPicker(_) => Ctx::TagPicker,
                Popup::FormTagPicker(_) => Ctx::FormTagPicker,
                Popup::TextPrompt(_) => Ctx::TextPrompt,
                Popup::Archive(_) => Ctx::Archive,
                Popup::Help => Ctx::Help,
            };
        }
        match self.pane {
            Pane::Board => Ctx::Board,
            Pane::Notes => Ctx::Notes,
        }
    }

    /// The active live-typed-or-committed search query, or `None` when no
    /// filter should apply. Live while typing (`Mode::Search`), and stays
    /// applied after `Enter` commits it (`search_active`); an empty
    /// pattern is never treated as an active filter.
    pub fn search_query(&self) -> Option<String> {
        if (self.mode == Mode::Search || self.search_active) && !self.search.is_empty() {
            Some(self.search.clone())
        } else {
            None
        }
    }

    /// The task at the focused column's current selection -- `None` when
    /// the column is empty *or* the selected row is hidden by an active
    /// search filter. Every task-scoped action (edit, delete, open in
    /// editor, move column, the status/priority/tag dropdowns) goes
    /// through this, so none of them can act on a row the screen isn't
    /// showing.
    fn selected_task(&self) -> Option<&Task> {
        let col = &self.columns[self.focused];
        let task = col.tasks.get(col.selected)?;
        match self.search_query() {
            Some(q) if !task_matches_query(task, &q) => None,
            _ => Some(task),
        }
    }

    /// Same rule as `selected_task`, for the notes list.
    fn selected_note(&self) -> Option<&Note> {
        let note = self.notes.get(self.notes_selected)?;
        match self.search_query() {
            Some(q) if !note_matches_query(note, &q) => None,
            _ => Some(note),
        }
    }

    /// Handles one key press: while any popup is open, keys go to the top
    /// of the popup stack; otherwise Command/Search text entry is routed
    /// directly, or the key is resolved to an `Action` via the keymap and
    /// dispatched.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if !self.popups.is_empty() {
            self.route_key_to_popup(key);
            return;
        }

        match self.mode {
            Mode::Command => {
                self.handle_command_key(key);
                return;
            }
            Mode::Search => {
                self.handle_search_key(key);
                return;
            }
            _ => {}
        }

        let action = keymap::resolve(self.mode, self.pane, self.search_active, key, &mut self.pending);
        self.dispatch(action);
    }

    /// Routes one key press to the top of the popup stack. Enter on an
    /// open form's Body field is intercepted here rather than delegated:
    /// it needs to suspend the TUI to run an external editor, which no
    /// popup has a handle to do.
    fn route_key_to_popup(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(Popup::Form(f)) = self.popups.last() {
            if f.field == Field::Body && key.code == KeyCode::Enter && !ctrl {
                self.pending_edit = Some((EditTarget::FormBody, f.body.clone()));
                return;
            }
            if f.field == Field::Tags && key.code == KeyCode::Enter && !ctrl {
                self.open_form_tag_picker();
                return;
            }
        }

        // The board switcher (a `Board`-target dropdown) gets a handful of
        // extra keys the generic dropdown handler doesn't know about:
        // `a`/`A` create a board, `dd`/`D` delete the highlighted one. This
        // is intercepted here rather than in `handle_dropdown_key` because
        // `dd` needs state (`board_switcher_pending_d`) that only `App`
        // carries across key presses, and because creating or deleting a
        // board needs `self.db`, which no popup has a handle to.
        if let Some(Popup::Dropdown(d)) = self.popups.last() {
            if d.target == DropdownTarget::Board && !ctrl {
                let highlighted = d.items.get(d.selected).cloned();
                match key.code {
                    KeyCode::Char('d') => {
                        if self.board_switcher_pending_d {
                            self.board_switcher_pending_d = false;
                            if let Some(item) = highlighted {
                                // Pop the switcher first: `begin_delete_board`
                                // either pushes a `Confirm` (which must land
                                // on an empty stack so its own Submit routes
                                // to `apply_confirmed`, not into a leftover
                                // `Dropdown` beneath it) or only sets a
                                // refusal message, closing the switcher
                                // either way -- the same "closes with a
                                // message" pattern as `a`/`A`.
                                self.popups.pop();
                                self.begin_delete_board(item.id);
                            }
                        } else {
                            self.board_switcher_pending_d = true;
                        }
                        return;
                    }
                    KeyCode::Char('D') => {
                        self.board_switcher_pending_d = false;
                        if let Some(item) = highlighted {
                            self.popups.pop();
                            self.begin_delete_board(item.id);
                        }
                        return;
                    }
                    KeyCode::Char('a') => {
                        self.board_switcher_pending_d = false;
                        self.popups.pop();
                        self.pending_action = Some(PendingPopupAction::NewBoardPlain);
                        self.popups.push(Popup::TextPrompt(TextPromptState {
                            prompt: "New board name".to_string(),
                            input: String::new(),
                            error: None,
                        }));
                        return;
                    }
                    KeyCode::Char('A') => {
                        self.board_switcher_pending_d = false;
                        self.popups.pop();
                        self.pending_action = Some(PendingPopupAction::NewBoardLocked);
                        self.popups.push(Popup::TextPrompt(TextPromptState {
                            prompt: "New locked board name".to_string(),
                            input: String::new(),
                            error: None,
                        }));
                        return;
                    }
                    _ => self.board_switcher_pending_d = false,
                }
            }
        }

        let outcome = self
            .popups
            .last_mut()
            .expect("route_key_to_popup called with a non-empty stack")
            .handle_key(key);
        self.apply_popup_outcome(outcome);
    }

    fn apply_popup_outcome(&mut self, outcome: PopupOutcome) {
        match outcome {
            PopupOutcome::Consumed => {}
            PopupOutcome::Close => {
                self.popups.pop();
                if self.popups.is_empty() {
                    self.pending_action = None;
                }
            }
            PopupOutcome::Push(p) => self.popups.push(*p),
            PopupOutcome::Submit(value) => {
                self.popups.pop();
                if let Some(top) = self.popups.last_mut() {
                    top.receive(value);
                } else {
                    self.apply_top_level_value(value);
                }
            }
            PopupOutcome::TagPicker(action) => self.apply_tag_picker_action(action),
            PopupOutcome::FormTagPicker(action) => self.apply_form_tag_picker_action(action),
            PopupOutcome::OpenArchivedTask(id) => self.open_archived_task_in_form(id),
        }
    }

    fn apply_top_level_value(&mut self, value: PopupValue) {
        match value {
            PopupValue::Form(draft) => self.apply_task_draft(draft),
            PopupValue::Selected(item) => self.apply_selected(item),
            PopupValue::Confirmed(yes) => self.apply_confirmed(yes),
            PopupValue::Passphrase(pw) => self.apply_passphrase(pw),
            PopupValue::Text(text) => self.apply_text(text),
        }
    }

    fn apply_selected(&mut self, item: SelectItem) {
        let Some(action) = self.pending_action.take() else {
            return;
        };
        match action {
            PendingPopupAction::MoveTaskColumn(id) => {
                let store = self.store();
                let Ok(before) = store.get_task(id) else {
                    self.message = Some("task no longer exists".to_string());
                    return;
                };
                let Some(new_status) = Status::ALL.get(item.id as usize).copied() else {
                    return;
                };
                if store.move_task(id, new_status, i64::MAX).is_ok() {
                    let after_pos = store.get_task(id).map(|t| t.position).unwrap_or(0);
                    self.undo.push(Command::MoveTask {
                        id,
                        from: (before.status, before.position),
                        to: (new_status, after_pos),
                    });
                    self.message = Some(format!("moved to {}", status_label(new_status)));
                    let _ = self.reload();
                } else {
                    self.message = Some("move failed".to_string());
                }
            }
            PendingPopupAction::SetPriority(id) => {
                let store = self.store();
                let Ok(before) = store.get_task(id) else {
                    self.message = Some("task no longer exists".to_string());
                    return;
                };
                let Some(new_priority) = Priority::ALL.iter().find(|p| **p as i64 == item.id).copied() else {
                    return;
                };
                let before_patch = TaskPatch { priority: Some(before.priority), ..Default::default() };
                let after_patch = TaskPatch { priority: Some(new_priority), ..Default::default() };
                if store.update_task(id, after_patch.clone()).is_ok() {
                    self.undo.push(Command::UpdateTask { id, before: before_patch, after: after_patch });
                    self.message = Some(format!("priority set to {}", new_priority.as_str()));
                    let _ = self.reload();
                } else {
                    self.message = Some("update failed".to_string());
                }
            }
            PendingPopupAction::PromoteNote(note_id) => {
                let Some(new_status) = Status::ALL.get(item.id as usize).copied() else {
                    return;
                };
                let store = self.store();
                match store.promote_note(note_id, new_status) {
                    Ok(task_id) => {
                        self.undo.push(Command::CreateTask { id: task_id, status: new_status });
                        self.message = Some("promoted to task".to_string());
                        let _ = self.reload();
                    }
                    Err(e) => self.message = Some(format!("promote failed: {e}")),
                }
            }
            PendingPopupAction::SwitchBoard => self.switch_to_board(item.id),
            PendingPopupAction::BoardMenu => self.apply_board_menu_selection(item.id),
            PendingPopupAction::SelectBoardToDelete => self.begin_delete_board(item.id),
            _ => {}
        }
    }

    fn apply_confirmed(&mut self, yes: bool) {
        let Some(action) = self.pending_action.take() else {
            return;
        };
        if !yes {
            self.message = Some("cancelled".to_string());
            return;
        }
        match action {
            PendingPopupAction::ConfirmDeleteTask(id) => self.delete_task_now(id),
            PendingPopupAction::ConfirmDeleteNote(id) => self.delete_note_now(id),
            PendingPopupAction::ConfirmDeleteTag(id) => match self.store().delete_tag(id) {
                Ok(()) => {
                    self.message = Some("tag deleted".to_string());
                    let _ = self.reload();
                }
                Err(e) => self.message = Some(format!("delete failed: {e}")),
            },
            PendingPopupAction::ConfirmCreateLockedBoard { name, passphrase } => {
                self.create_locked_board_now(&name, &passphrase);
            }
            PendingPopupAction::ConfirmDeleteBoard(id) => self.delete_board_now(id),
            _ => {}
        }
    }

    fn apply_passphrase(&mut self, pw: String) {
        let Some(action) = self.pending_action.take() else {
            return;
        };
        match action {
            PendingPopupAction::UnlockBoard(board_id) => self.try_unlock_board(board_id, pw),
            PendingPopupAction::NewLockedBoardPass1 { name } => {
                self.pending_action = Some(PendingPopupAction::NewLockedBoardPass2 { name: name.clone(), first: pw });
                self.popups.push(Popup::Passphrase(PassphraseState {
                    prompt: format!("Confirm passphrase for \"{name}\""),
                    input: String::new(),
                    error: None,
                }));
            }
            PendingPopupAction::NewLockedBoardPass2 { name, first } => {
                if pw != first {
                    self.message = Some("passphrases did not match".to_string());
                    self.pending_action = Some(PendingPopupAction::NewLockedBoardPass1 { name: name.clone() });
                    self.popups.push(Popup::Passphrase(PassphraseState {
                        prompt: format!("Passphrase for \"{name}\""),
                        input: String::new(),
                        error: Some("passphrases did not match; try again".to_string()),
                    }));
                    return;
                }
                self.pending_action = Some(PendingPopupAction::ConfirmCreateLockedBoard { name, passphrase: pw });
                self.popups.push(Popup::Confirm(ConfirmState {
                    message: "A forgotten passphrase makes this board's data permanently unrecoverable. Continue?"
                        .to_string(),
                }));
            }
            _ => {}
        }
    }

    /// Applies the text submitted by a one-line `TextPrompt`: which
    /// `PendingPopupAction` was recorded before the prompt was pushed says
    /// what the text is for (new tag name, rename-tag name, or a
    /// quick-capture note body).
    fn apply_text(&mut self, text: String) {
        let Some(action) = self.pending_action.take() else {
            return;
        };
        match action {
            PendingPopupAction::NewTagForTask(task_id) => {
                let store = self.store();
                match store.upsert_tag(text.trim(), None) {
                    Ok(tag_id) => {
                        let mut applied: Vec<TagId> =
                            store.tags_for_task(task_id).unwrap_or_default().into_iter().map(|t| t.id).collect();
                        if !applied.contains(&tag_id) {
                            applied.push(tag_id);
                        }
                        match store.set_task_tags(task_id, &applied) {
                            Ok(()) => {
                                self.message = Some("tag created".to_string());
                                let _ = self.reload();
                            }
                            Err(e) => self.message = Some(format!("tag apply failed: {e}")),
                        }
                    }
                    Err(e) => self.message = Some(format!("{e}")),
                }
            }
            PendingPopupAction::RenameTag(tag_id) => match self.store().rename_tag(tag_id, text.trim()) {
                Ok(()) => {
                    self.message = Some("tag renamed".to_string());
                    let _ = self.reload();
                }
                Err(e) => self.message = Some(format!("{e}")),
            },
            PendingPopupAction::QuickCaptureNote => {
                if text.trim().is_empty() {
                    self.message = Some("empty capture discarded".to_string());
                    return;
                }
                match self.store().create_note(&text) {
                    Ok(id) => {
                        self.undo.push(Command::CreateNote { id });
                        self.message = Some("note captured".to_string());
                        let _ = self.reload();
                    }
                    Err(e) => self.message = Some(format!("capture failed: {e}")),
                }
            }
            PendingPopupAction::NewBoardPlain => self.begin_plain_board_flow(text),
            PendingPopupAction::NewBoardLocked => self.begin_locked_board_flow(text),
            PendingPopupAction::RenameCurrentBoard => {
                let name = text.trim().to_string();
                if name.is_empty() {
                    self.message = Some("usage: :board rename <name>".to_string());
                    return;
                }
                match self.db.rename_board(self.board.id, &name) {
                    Ok(()) => {
                        self.board.name = name.clone();
                        self.message = Some(format!("renamed to \"{name}\""));
                    }
                    Err(e) => self.message = Some(format!("rename failed: {e}")),
                }
            }
            _ => {}
        }
    }

    /// Opens the archive browser: every Done task on the current board
    /// whose `completed_at` is more than `ARCHIVE_AFTER_DAYS` days old,
    /// newest completion first. Queried fresh from the store every time it
    /// opens -- `self.columns` deliberately excludes archived rows, so
    /// they are not sitting in memory anywhere else -- the same "ask the
    /// store fresh" pattern `open_tag_picker` uses.
    fn open_archive_browser(&mut self) {
        let store = self.store();
        let now = chrono::Utc::now().timestamp();
        let mut archived: Vec<Task> = match store.list_tasks(Status::Done) {
            Ok(tasks) => tasks.into_iter().filter(|t| is_archived(t, now)).collect(),
            Err(e) => {
                self.message = Some(format!("failed to load archive: {e}"));
                return;
            }
        };
        archived.sort_by_key(completed_sort_key);
        self.popups.push(Popup::Archive(ArchiveBrowserState { tasks: archived, filter: String::new(), selected: 0 }));
        self.message = None;
    }

    /// Pops the archive browser and pushes the edit form for `id`, fetched
    /// fresh from the store so its tags are current. The browser itself is
    /// replaced on the stack rather than left underneath the form: the
    /// form's eventual Ctrl-S submit must reach `apply_task_draft` through
    /// `apply_top_level_value`, and `Popup::receive` only forwards a value
    /// into a `Form`, so leaving the browser in place would silently
    /// swallow the save.
    fn open_archived_task_in_form(&mut self, id: TaskId) {
        self.popups.pop();
        let store = self.store();
        match store.get_task(id) {
            Ok(task) => {
                let tags = task.tags.iter().map(|t| t.name.clone()).collect();
                self.popups.push(Popup::Form(FormState::edit_task(
                    task.id,
                    task.title,
                    task.body,
                    task.priority,
                    task.status,
                    task.start_date,
                    task.time_expected,
                    task.deadline,
                    tags,
                )));
            }
            Err(_) => self.message = Some("task no longer exists".to_string()),
        }
    }

    /// Opens the tag picker for `task_id`: every board tag, with the
    /// task's current tags marked.
    fn open_tag_picker(&mut self, task_id: TaskId) {
        let store = self.store();
        let tags = store.list_tags().unwrap_or_default();
        let applied = store.tags_for_task(task_id).unwrap_or_default().into_iter().map(|t| t.id).collect();
        self.popups.push(Popup::TagPicker(TagPickerState { task_id, tags, applied, selected: 0 }));
    }

    /// Applies a key press handled inside the tag picker. `Toggle` persists
    /// in place and leaves the picker open; `New`/`Rename`/`Delete` replace
    /// it with a one-line text prompt or a delete confirmation.
    fn apply_tag_picker_action(&mut self, action: TagPickerAction) {
        let Some(Popup::TagPicker(t)) = self.popups.last() else {
            return;
        };
        let task_id = t.task_id;
        match action {
            TagPickerAction::Toggle(tag_id) => self.toggle_tag(task_id, tag_id),
            TagPickerAction::New => {
                self.popups.pop();
                self.pending_action = Some(PendingPopupAction::NewTagForTask(task_id));
                self.popups.push(Popup::TextPrompt(TextPromptState {
                    prompt: "New tag name".to_string(),
                    input: String::new(),
                    error: None,
                }));
            }
            TagPickerAction::Rename(tag_id) => {
                let current_name =
                    t.tags.iter().find(|tag| tag.id == tag_id).map(|tag| tag.name.clone()).unwrap_or_default();
                self.popups.pop();
                self.pending_action = Some(PendingPopupAction::RenameTag(tag_id));
                self.popups.push(Popup::TextPrompt(TextPromptState {
                    prompt: "Rename tag".to_string(),
                    input: current_name,
                    error: None,
                }));
            }
            TagPickerAction::Delete(tag_id) => {
                let name = t.tags.iter().find(|tag| tag.id == tag_id).map(|tag| tag.name.clone()).unwrap_or_default();
                self.popups.pop();
                self.pending_action = Some(PendingPopupAction::ConfirmDeleteTag(tag_id));
                self.popups.push(Popup::Confirm(ConfirmState {
                    message: format!("delete tag \"{name}\"? this removes it from every task"),
                }));
            }
        }
    }

    /// Opens the form-scoped tag picker for the Tags field of the form
    /// currently on top of the popup stack: every board tag, with the
    /// form's own (in-memory, unsaved) `tags` names marked. Toggling
    /// writes straight into that `Vec<String>` -- nothing reaches the
    /// store until the form itself is submitted (Ctrl-S).
    fn open_form_tag_picker(&mut self) {
        let mut all_tags: Vec<String> = self.store().list_tags().unwrap_or_default().into_iter().map(|t| t.name).collect();
        let Some(Popup::Form(f)) = self.popups.last() else { return };
        let applied = f.tags.clone();
        for name in &applied {
            if !all_tags.contains(name) {
                all_tags.push(name.clone());
            }
        }
        self.popups.push(Popup::FormTagPicker(FormTagPickerState { all_tags, applied, selected: 0 }));
    }

    /// Applies a key press handled inside the form-scoped tag picker.
    /// `Toggle` mutates the form directly beneath it on the stack and
    /// leaves the picker open; `New` replaces it with a one-line text
    /// prompt whose result reaches the form via `Popup::receive`.
    fn apply_form_tag_picker_action(&mut self, action: FormTagPickerAction) {
        match action {
            FormTagPickerAction::Toggle(idx) => {
                let Some(Popup::FormTagPicker(t)) = self.popups.last() else { return };
                let Some(name) = t.all_tags.get(idx).cloned() else { return };
                let len = self.popups.len();
                if len < 2 {
                    return;
                }
                let Some(Popup::Form(f)) = self.popups.get_mut(len - 2) else { return };
                if let Some(pos) = f.tags.iter().position(|existing| *existing == name) {
                    f.tags.remove(pos);
                } else {
                    f.tags.push(name);
                }
                let applied = f.tags.clone();
                if let Some(Popup::FormTagPicker(t)) = self.popups.last_mut() {
                    t.applied = applied;
                }
            }
            FormTagPickerAction::New => {
                self.popups.pop();
                self.popups.push(Popup::TextPrompt(TextPromptState {
                    prompt: "New tag name".to_string(),
                    input: String::new(),
                    error: None,
                }));
            }
        }
    }

    /// Toggles `tag_id` on `task_id` (persisted via `set_task_tags`) and,
    /// if the tag picker is still the top popup, refreshes its `applied`
    /// list so the `*` marker updates without closing it.
    fn toggle_tag(&mut self, task_id: TaskId, tag_id: TagId) {
        let store = self.store();
        let mut applied: Vec<TagId> =
            store.tags_for_task(task_id).unwrap_or_default().into_iter().map(|t| t.id).collect();
        if let Some(pos) = applied.iter().position(|id| *id == tag_id) {
            applied.remove(pos);
        } else {
            applied.push(tag_id);
        }
        match store.set_task_tags(task_id, &applied) {
            Ok(()) => {
                let _ = self.reload();
                if let Some(Popup::TagPicker(t)) = self.popups.last_mut() {
                    t.applied = applied;
                }
            }
            Err(e) => self.message = Some(format!("tag update failed: {e}")),
        }
    }

    fn apply_task_draft(&mut self, draft: TaskDraft) {
        let store = self.store();
        match draft.kind {
            FormKind::NewTask(_) => {
                let status = draft.status;
                let new_task = NewTask {
                    title: draft.title.clone(),
                    body: draft.body.clone(),
                    status,
                    priority: draft.priority,
                    start_date: draft.start_date,
                    time_expected: draft.time_expected,
                    deadline: draft.deadline,
                };
                match store.create_task(new_task) {
                    Ok(id) => {
                        // Tags are set by name and are not part of this
                        // undo command -- the card-level `t` toggle
                        // (`App::toggle_tag`) is likewise not undoable, so
                        // this matches existing behaviour rather than
                        // adding a new undo path just for the form. Done
                        // before the undo push below, since `store`
                        // (borrowed from `self`) must not still be alive
                        // when `self.undo` is mutated.
                        let tags_result = apply_draft_tags(&store, id, &draft.tags);
                        self.undo.push(Command::CreateTask { id, status });
                        match tags_result {
                            Ok(()) => self.message = Some(format!("created \"{}\"", draft.title)),
                            Err(e) => {
                                self.message = Some(format!("created \"{}\" (tags failed: {e})", draft.title))
                            }
                        }
                        let _ = self.reload();
                        self.select_task(status, id);
                    }
                    Err(e) => self.message = Some(format!("create failed: {e}")),
                }
            }
            FormKind::EditTask => {
                let Some(id) = draft.editing_id else { return };
                let Ok(before_task) = store.get_task(id) else {
                    self.message = Some("task no longer exists".to_string());
                    return;
                };
                let before = TaskPatch {
                    title: Some(before_task.title.clone()),
                    body: Some(before_task.body.clone()),
                    status: None,
                    priority: Some(before_task.priority),
                    start_date: Some(before_task.start_date),
                    time_expected: Some(before_task.time_expected),
                    deadline: Some(before_task.deadline),
                };
                let after = TaskPatch {
                    title: Some(draft.title.clone()),
                    body: Some(draft.body.clone()),
                    status: None,
                    priority: Some(draft.priority),
                    start_date: Some(draft.start_date),
                    time_expected: Some(draft.time_expected),
                    deadline: Some(draft.deadline),
                };
                if let Err(e) = store.update_task(id, after.clone()) {
                    self.message = Some(format!("update failed: {e}"));
                    return;
                }
                // Status moves through `move_task` (not the patch above),
                // the same as `H`/`L`/`m`/`S`, so dense per-column
                // positions stay correct; its own undo command is pushed
                // alongside the field update's, once `store` (which
                // borrows all of `self`) is done being used -- pushing to
                // `self.undo` while `store` is still alive does not
                // borrow-check.
                let status_move = if draft.status != before_task.status && store.move_task(id, draft.status, i64::MAX).is_ok() {
                    let after_pos = store.get_task(id).map(|t| t.position).unwrap_or(0);
                    Some((draft.status, after_pos))
                } else {
                    None
                };
                // Tags are set by name and are not undoable -- see the
                // comment in the `NewTask` arm above. Done before the undo
                // pushes below, for the same borrow-ordering reason.
                let tags_result = apply_draft_tags(&store, id, &draft.tags);
                self.undo.push(Command::UpdateTask { id, before, after });
                if let Some((new_status, after_pos)) = status_move {
                    self.undo.push(Command::MoveTask {
                        id,
                        from: (before_task.status, before_task.position),
                        to: (new_status, after_pos),
                    });
                }
                match tags_result {
                    Ok(()) => self.message = Some(format!("updated \"{}\"", draft.title)),
                    Err(e) => self.message = Some(format!("updated \"{}\" (tags failed: {e})", draft.title)),
                }
                let _ = self.reload();
            }
            FormKind::NewNote => {
                let body = combine_note_body(&draft.title, &draft.body);
                match store.create_note(&body) {
                    Ok(id) => {
                        self.undo.push(Command::CreateNote { id });
                        self.message = Some("note captured".to_string());
                        let _ = self.reload();
                    }
                    Err(e) => self.message = Some(format!("create failed: {e}")),
                }
            }
            FormKind::EditNote => {
                let Some(id) = draft.editing_id else { return };
                let Some(before_note) = store.list_notes().unwrap_or_default().into_iter().find(|n| n.id == id)
                else {
                    self.message = Some("note no longer exists".to_string());
                    return;
                };
                let after = combine_note_body(&draft.title, &draft.body);
                match store.update_note(id, &after) {
                    Ok(()) => {
                        self.undo.push(Command::UpdateNote { id, before: before_note.body, after });
                        self.message = Some("note updated".to_string());
                        let _ = self.reload();
                    }
                    Err(e) => self.message = Some(format!("update failed: {e}")),
                }
            }
        }
    }

    fn select_task(&mut self, status: Status, id: TaskId) {
        let idx = Status::ALL.iter().position(|s| *s == status).unwrap_or(0);
        if let Some(pos) = self.columns[idx].tasks.iter().position(|t| t.id == id) {
            self.columns[idx].selected = pos;
            self.pane = Pane::Board;
            self.focused = idx;
        }
    }

    fn delete_task_now(&mut self, id: TaskId) {
        let store = self.store();
        let snapshot = match store.get_task(id) {
            Ok(t) => t,
            Err(_) => {
                self.message = Some("task no longer exists".to_string());
                return;
            }
        };
        match store.delete_task(id) {
            Ok(()) => {
                let title = snapshot.title.clone();
                self.undo.push(Command::DeleteTask { snapshot, tags: Vec::new() });
                self.message = Some(format!("deleted \"{title}\""));
                let _ = self.reload();
            }
            Err(e) => self.message = Some(format!("delete failed: {e}")),
        }
    }

    fn delete_note_now(&mut self, id: NoteId) {
        let store = self.store();
        let notes = store.list_notes().unwrap_or_default();
        let Some(snapshot) = notes.into_iter().find(|n| n.id == id) else {
            self.message = Some("note no longer exists".to_string());
            return;
        };
        match store.delete_note(id) {
            Ok(()) => {
                self.undo.push(Command::DeleteNote { snapshot });
                self.message = Some("deleted note".to_string());
                let _ = self.reload();
            }
            Err(e) => self.message = Some(format!("delete failed: {e}")),
        }
    }

    /// Suspends the TUI to run the configured editor on `initial`, and
    /// returns its (possibly unchanged, if the edit was discarded)
    /// contents. Builds its own transient `Terminal` over the process's
    /// stdout: `event.rs`'s event loop -- which owns the real one -- is
    /// outside this phase's owned files (see the report).
    /// Which binary to launch for external editing. Resolution order is
    /// config > nvim > $VISUAL > $EDITOR > vim; nvim deliberately outranks
    /// the environment variables.
    pub fn editor_command(&self) -> Result<String> {
        config::resolve_editor(
            &self.config,
            &config::path_lookup,
            std::env::var("VISUAL").ok().as_deref(),
            std::env::var("EDITOR").ok().as_deref(),
        )
    }

    /// Takes a pending external-edit request, if a key press made one.
    pub fn take_pending_edit(&mut self) -> Option<(EditTarget, String)> {
        self.pending_edit.take()
    }

    /// Best-effort title for the tmux floating editor's popup border:
    /// `EditTarget::Task`'s current title from the store, or the open
    /// form's `title` field for `EditTarget::FormBody`. `None` when
    /// neither is available -- the task may have been deleted between the
    /// key press and this call, or the form's title is still blank --
    /// in which case the caller falls back to "body".
    pub fn pending_edit_title(&self, target: EditTarget) -> Option<String> {
        match target {
            EditTarget::Task(id) => self.store().get_task(id).ok().map(|t| t.title),
            EditTarget::FormBody => match self.popups.last() {
                Some(Popup::Form(f)) if !f.title.trim().is_empty() => Some(f.title.clone()),
                _ => None,
            },
        }
    }

    /// Reports that an external editor could not be started or run.
    pub fn report_editor_error(&mut self, e: impl std::fmt::Display) {
        self.message = Some(format!("editor failed: {e}"));
    }

    /// Applies text returned by an external editor.
    pub fn apply_edited_text(&mut self, target: EditTarget, text: String) {
        match target {
            EditTarget::Task(id) => {
                let before = self.store().get_task(id).map(|t| t.body).unwrap_or_default();
                self.save_task_body(id, before, text);
            }
            EditTarget::FormBody => {
                if let Some(Popup::Form(f)) = self.popups.last_mut() {
                    f.body = text;
                }
            }
        }
    }

    fn save_task_body(&mut self, id: TaskId, before_body: String, after_body: String) {
        if before_body == after_body {
            return;
        }
        let store = self.store();
        let before = TaskPatch { body: Some(before_body), ..Default::default() };
        let after = TaskPatch { body: Some(after_body), ..Default::default() };
        match store.update_task(id, after.clone()) {
            Ok(()) => {
                self.undo.push(Command::UpdateTask { id, before, after });
                self.message = Some("saved".to_string());
                let _ = self.reload();
            }
            Err(e) => self.message = Some(format!("save failed: {e}")),
        }
    }

    fn move_task_column(&mut self, delta: i32) {
        let Some(task) = self.selected_task().cloned() else {
            self.message = Some("no task selected".to_string());
            return;
        };
        let new_status = if delta < 0 { task.status.prev() } else { task.status.next() };
        if new_status == task.status {
            self.message = Some("already at the end".to_string());
            return;
        }
        let store = self.store();
        match store.move_task(task.id, new_status, i64::MAX) {
            Ok(()) => {
                let after_pos = store.get_task(task.id).map(|t| t.position).unwrap_or(0);
                self.undo.push(Command::MoveTask {
                    id: task.id,
                    from: (task.status, task.position),
                    to: (new_status, after_pos),
                });
                self.message = Some(format!("moved to {}", status_label(new_status)));
                let _ = self.reload();
            }
            Err(e) => self.message = Some(format!("move failed: {e}")),
        }
    }

    fn reorder_selected(&mut self, delta: i32) {
        if self.selected_task().is_none() {
            self.message = Some("no task selected".to_string());
            return;
        }
        let idx = self.focused;
        let status = Status::ALL[idx];
        let visible = self.visible_task_indices(idx);
        let col = &self.columns[idx];
        let sel = col.selected;
        let Some(pos) = visible.iter().position(|&i| i == sel) else {
            self.message = Some("no task selected".to_string());
            return;
        };
        let target_pos = pos as i32 + delta;
        if target_pos < 0 || target_pos as usize >= visible.len() {
            self.message = Some("already at the end".to_string());
            return;
        }
        let target = visible[target_pos as usize];

        if !same_sort_bucket(idx, &col.tasks[sel], &col.tasks[target]) {
            self.message = Some(sort_bucket_mismatch_message(idx));
            return;
        }

        let mut ordered: Vec<TaskId> = col.tasks.iter().map(|t| t.id).collect();
        ordered.swap(sel, target);
        let moved_id = col.tasks[sel].id;
        let from_pos = col.tasks[sel].position;
        let to_pos = col.tasks[target].position;

        let store = self.store();
        match store.reorder(status, &ordered) {
            Ok(()) => {
                self.columns[idx].selected = target;
                self.undo.push(Command::MoveTask { id: moved_id, from: (status, from_pos), to: (status, to_pos) });
                self.message = Some("reordered".to_string());
                let _ = self.reload();
            }
            Err(e) => self.message = Some(format!("reorder failed: {e}")),
        }
    }

    /// Computes and applies the inverse of `cmd` against storage, and
    /// returns the command that undoes *that* -- i.e. what should go on
    /// the opposite stack. Used identically by `do_undo` (pop from undo,
    /// push result to redo) and `do_redo` (pop from redo, push result to
    /// undo), which is what makes re-creating a deleted row under a new id
    /// keep working under repeated undo/redo: each inversion captures
    /// whatever id the row currently has.
    fn apply_inverse(&mut self, cmd: Command) -> std::result::Result<Command, StorageError> {
        let store = self.store();
        match cmd {
            Command::CreateTask { id, status: _ } => {
                let snapshot = store.get_task(id)?;
                store.delete_task(id)?;
                Ok(Command::DeleteTask { snapshot, tags: Vec::new() })
            }
            Command::DeleteTask { snapshot, .. } => {
                let new_id = store.create_task(NewTask {
                    title: snapshot.title.clone(),
                    body: snapshot.body.clone(),
                    status: snapshot.status,
                    priority: snapshot.priority,
                    start_date: snapshot.start_date,
                    time_expected: snapshot.time_expected,
                    deadline: snapshot.deadline,
                })?;
                store.move_task(new_id, snapshot.status, snapshot.position)?;
                Ok(Command::CreateTask { id: new_id, status: snapshot.status })
            }
            Command::UpdateTask { id, before, after } => {
                store.update_task(id, before.clone())?;
                Ok(Command::UpdateTask { id, before: after, after: before })
            }
            Command::MoveTask { id, from, to } => {
                store.move_task(id, from.0, from.1)?;
                Ok(Command::MoveTask { id, from: to, to: from })
            }
            Command::CreateNote { id } => {
                let snapshot = store
                    .list_notes()?
                    .into_iter()
                    .find(|n| n.id == id)
                    .ok_or(StorageError::NotFound)?;
                store.delete_note(id)?;
                Ok(Command::DeleteNote { snapshot })
            }
            Command::DeleteNote { snapshot } => {
                let new_id = store.create_note(&snapshot.body)?;
                Ok(Command::CreateNote { id: new_id })
            }
            Command::UpdateNote { id, before, after } => {
                store.update_note(id, &before)?;
                Ok(Command::UpdateNote { id, before: after, after: before })
            }
        }
    }

    fn do_undo(&mut self) {
        let Some(cmd) = self.undo.undo() else {
            self.message = Some("nothing to undo".to_string());
            return;
        };
        match self.apply_inverse(cmd) {
            Ok(redo_cmd) => {
                self.undo.push_redo(redo_cmd);
                self.message = Some("undone".to_string());
                let _ = self.reload();
            }
            Err(_) => self.message = Some("undo failed: item no longer exists".to_string()),
        }
    }

    fn do_redo(&mut self) {
        let Some(cmd) = self.undo.redo() else {
            self.message = Some("nothing to redo".to_string());
            return;
        };
        match self.apply_inverse(cmd) {
            Ok(undo_cmd) => {
                self.undo.push_undo(undo_cmd);
                self.message = Some("redone".to_string());
                let _ = self.reload();
            }
            Err(_) => self.message = Some("redo failed: item no longer exists".to_string()),
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::Enter => {
                let cmd = self.cmdline.trim().to_string();
                self.cmdline.clear();
                self.mode = Mode::Normal;
                self.run_command(&cmd);
            }
            KeyCode::Esc => {
                self.cmdline.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cmdline.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.cmdline.pop();
            }
            KeyCode::Char(c) => self.cmdline.push(c),
            _ => {}
        }
    }

    /// Parses and runs one command-mode line (without the leading `:`).
    /// Recognises `q`/`quit`, `lock`, `nohl`, and the `board <sub>` family;
    /// anything else reports itself as not implemented, same as before
    /// this phase.
    fn run_command(&mut self, cmd: &str) {
        if cmd.is_empty() {
            return;
        }
        if cmd == "q" || cmd == "quit" {
            self.should_quit = true;
            return;
        }
        if cmd == "lock" {
            self.lock_current_board();
            return;
        }
        if cmd == "nohl" {
            self.search.clear();
            self.search_active = false;
            self.message = Some("search cleared".to_string());
            return;
        }
        if cmd == "e" || cmd == "reload" {
            self.dispatch(Action::Refresh);
            return;
        }
        if cmd == "board" {
            self.open_board_menu();
            return;
        }
        if let Some(rest) = cmd.strip_prefix("board ") {
            self.run_board_command(rest.trim());
            return;
        }
        self.message = Some(format!(":{cmd} not implemented yet"));
    }

    fn run_board_command(&mut self, rest: &str) {
        let mut parts = rest.splitn(2, ' ');
        let sub = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();
        match sub {
            "new" => self.start_new_board(arg),
            "rename" => self.rename_current_board(arg),
            "delete" => self.start_delete_board(arg),
            _ => self.message = Some(format!(":board {rest} not implemented yet")),
        }
    }

    /// `:board new <name> [locked]`. A locked board's passphrase is
    /// collected afterward, through two passphrase popups plus an
    /// unrecoverable-data confirmation -- see `apply_passphrase` and
    /// `apply_confirmed`.
    fn start_new_board(&mut self, arg: &str) {
        let mut tokens = arg.split_whitespace();
        let Some(name) = tokens.next() else {
            self.message = Some("usage: :board new <name> [locked]".to_string());
            return;
        };
        let name = name.to_string();
        let locked = tokens.next().map(|t| t.eq_ignore_ascii_case("locked")).unwrap_or(false);
        if locked {
            self.begin_locked_board_flow(name);
        } else {
            self.begin_plain_board_flow(name);
        }
    }

    /// Shared by `:board new <name>`, the board switcher's `a`, and the
    /// `:board` menu's "New board": creates a plain board once its name is
    /// confirmed free.
    fn begin_plain_board_flow(&mut self, name: String) {
        let name = name.trim().to_string();
        if name.is_empty() {
            self.message = Some("board name cannot be empty".to_string());
            return;
        }
        if matches!(self.db.get_board_by_name(&name), Ok(Some(_))) {
            self.message = Some(format!("board already exists: {name}"));
            return;
        }
        match self.db.create_board(&name, BoardKind::Plain) {
            Ok(id) => {
                self.switch_to_board(id);
                self.message = Some(format!("created board \"{name}\""));
            }
            Err(e) => self.message = Some(format!("create board failed: {e}")),
        }
    }

    /// Shared by `:board new <name> locked`, the board switcher's `A`, and
    /// the `:board` menu's "New locked board": checks the name is free,
    /// then starts the same two-passphrase-entry flow as before.
    fn begin_locked_board_flow(&mut self, name: String) {
        let name = name.trim().to_string();
        if name.is_empty() {
            self.message = Some("board name cannot be empty".to_string());
            return;
        }
        if matches!(self.db.get_board_by_name(&name), Ok(Some(_))) {
            self.message = Some(format!("board already exists: {name}"));
            return;
        }
        self.pending_action = Some(PendingPopupAction::NewLockedBoardPass1 { name: name.clone() });
        self.popups.push(Popup::Passphrase(PassphraseState {
            prompt: format!("Passphrase for \"{name}\""),
            input: String::new(),
            error: None,
        }));
    }

    fn rename_current_board(&mut self, arg: &str) {
        if arg.is_empty() {
            self.message = Some("usage: :board rename <name>".to_string());
            return;
        }
        match self.db.rename_board(self.board.id, arg) {
            Ok(()) => {
                self.board.name = arg.to_string();
                self.message = Some(format!("renamed to \"{arg}\""));
            }
            Err(e) => self.message = Some(format!("rename failed: {e}")),
        }
    }

    fn start_delete_board(&mut self, arg: &str) {
        if arg.is_empty() {
            self.message = Some("usage: :board delete <name>".to_string());
            return;
        }
        let Ok(Some(target)) = self.db.get_board_by_name(arg) else {
            self.message = Some(format!("no such board: {arg}"));
            return;
        };
        self.begin_delete_board(target.id);
    }

    /// Shared by `:board delete <name>`, the board switcher's `dd`/`D`,
    /// and the `:board` menu's "Delete board…": refuses to queue a
    /// delete for the current board or when it is the only board,
    /// otherwise pushes the same confirmation as before.
    fn begin_delete_board(&mut self, board_id: BoardId) {
        let boards = self.db.list_boards().unwrap_or_default();
        let Some(target) = boards.iter().find(|b| b.id == board_id) else {
            self.message = Some("board no longer exists".to_string());
            return;
        };
        if boards.len() <= 1 {
            self.message = Some("cannot delete the only board".to_string());
            return;
        }
        if board_id == self.board.id {
            self.message = Some("cannot delete the current board; switch away first".to_string());
            return;
        }
        self.pending_action = Some(PendingPopupAction::ConfirmDeleteBoard(board_id));
        self.popups.push(Popup::Confirm(ConfirmState {
            message: format!("delete board \"{}\"? this cannot be undone", target.name),
        }));
    }

    fn delete_board_now(&mut self, board_id: BoardId) {
        let boards = self.db.list_boards().unwrap_or_default();
        let Some(target) = boards.into_iter().find(|b| b.id == board_id) else {
            self.message = Some("board no longer exists".to_string());
            return;
        };
        let switching_away_from_current = target.id == self.board.id;

        if let Err(e) = self.db.delete_board(board_id) {
            self.message = Some(format!("delete failed: {e}"));
            return;
        }
        self.unlocked.remove(&board_id);
        if let Some(path) = &target.db_path {
            let _ = std::fs::remove_file(path);
        }

        if switching_away_from_current {
            let remaining = self.db.list_boards().unwrap_or_default();
            if let Some(next) = remaining.into_iter().find(|b| b.kind == BoardKind::Plain) {
                self.board = next;
                self.focused = 0;
                let _ = self.reload();
            }
        }
        self.message = Some(format!("deleted board \"{}\"", target.name));
    }

    fn create_locked_board_now(&mut self, name: &str, passphrase: &str) {
        let salt = crate::crypto::generate_salt();
        let kdf = crate::crypto::Kdf::default();
        let key = match crate::crypto::derive_key(passphrase, &salt, kdf) {
            Ok(k) => k,
            Err(e) => {
                self.message = Some(format!("key derivation failed: {e}"));
                return;
            }
        };
        let key_hex = crate::crypto::key_to_hex(&key);

        let board_id = match self.db.create_board(name, BoardKind::Locked) {
            Ok(id) => id,
            Err(e) => {
                self.message = Some(format!("create board failed: {e}"));
                return;
            }
        };

        let path = self.data_dir.join(format!("board-{board_id}.db"));

        if let Err(e) = LockedDb::create(&path, &key_hex, name, self.config.journal_mode) {
            let _ = self.db.delete_board(board_id);
            self.message = Some(format!("failed to create encrypted file: {e}"));
            return;
        }

        if let Err(e) =
            self.db.set_board_crypto(board_id, &path.to_string_lossy(), &salt, kdf.m_cost, kdf.t_cost, kdf.p_cost)
        {
            self.message = Some(format!("created board but failed to save its crypto metadata: {e}"));
            return;
        }

        self.message = Some(format!("created locked board \"{name}\""));
    }

    fn lock_current_board(&mut self) {
        if self.board.kind != BoardKind::Locked {
            self.message = Some("current board isn't locked".to_string());
            return;
        }
        self.unlocked.remove(&self.board.id);
        let boards = self.db.list_boards().unwrap_or_default();
        if let Some(plain) = boards.into_iter().find(|b| b.kind == BoardKind::Plain) {
            self.board = plain;
            self.pane = Pane::Board;
            self.focused = 0;
            self.message = Some(format!("locked; switched to \"{}\"", self.board.name));
            let _ = self.reload();
        } else {
            self.message = Some("locked (no plain board to switch to)".to_string());
        }
    }

    /// Opens the board switcher: a dropdown listing every board, locked
    /// ones marked. Selecting a plain board switches immediately;
    /// selecting a locked one pushes a passphrase prompt instead (or
    /// switches immediately if that board is already unlocked this
    /// session).
    fn open_board_switcher(&mut self) {
        let boards = match self.db.list_boards() {
            Ok(b) => b,
            Err(e) => {
                self.message = Some(format!("failed to list boards: {e}"));
                return;
            }
        };
        if boards.is_empty() {
            self.message = Some("no boards available".to_string());
            return;
        }
        let selected = boards.iter().position(|b| b.id == self.board.id).unwrap_or(0);
        let items = boards
            .iter()
            .map(|b| SelectItem {
                id: b.id,
                label: if b.kind == BoardKind::Locked { format!("{} [locked]", b.name) } else { b.name.clone() },
            })
            .collect();
        self.board_switcher_pending_d = false;
        self.pending_action = Some(PendingPopupAction::SwitchBoard);
        self.popups.push(Popup::Dropdown(DropdownState {
            title: "Boards (a new, A locked, dd/D delete)".to_string(),
            items,
            selected,
            target: DropdownTarget::Board,
        }));
    }

    /// Opens the bare `:board` menu: one action per row, `Enter` runs it.
    fn open_board_menu(&mut self) {
        let mut items = vec![
            SelectItem { id: BOARD_MENU_NEW, label: "New board".to_string() },
            SelectItem { id: BOARD_MENU_NEW_LOCKED, label: "New locked board".to_string() },
            SelectItem { id: BOARD_MENU_RENAME, label: "Rename current board".to_string() },
            SelectItem { id: BOARD_MENU_DELETE, label: "Delete board…".to_string() },
            SelectItem { id: BOARD_MENU_SWITCH, label: "Switch board…".to_string() },
        ];
        if self.board.kind == BoardKind::Locked && self.unlocked.contains_key(&self.board.id) {
            items.push(SelectItem { id: BOARD_MENU_LOCK, label: "Lock current board".to_string() });
        }
        items.push(SelectItem {
            id: BOARD_MENU_INFO,
            label: format!("Current: \"{}\" [{}]", self.board.name, self.board.kind.as_str()),
        });
        self.pending_action = Some(PendingPopupAction::BoardMenu);
        self.popups.push(Popup::Dropdown(DropdownState {
            title: "Board menu".to_string(),
            items,
            selected: 0,
            target: DropdownTarget::BoardMenu,
        }));
    }

    /// Applies the `id` of whichever row was selected in `open_board_menu`.
    fn apply_board_menu_selection(&mut self, id: i64) {
        match id {
            BOARD_MENU_NEW => {
                self.pending_action = Some(PendingPopupAction::NewBoardPlain);
                self.popups.push(Popup::TextPrompt(TextPromptState {
                    prompt: "New board name".to_string(),
                    input: String::new(),
                    error: None,
                }));
            }
            BOARD_MENU_NEW_LOCKED => {
                self.pending_action = Some(PendingPopupAction::NewBoardLocked);
                self.popups.push(Popup::TextPrompt(TextPromptState {
                    prompt: "New locked board name".to_string(),
                    input: String::new(),
                    error: None,
                }));
            }
            BOARD_MENU_RENAME => {
                self.pending_action = Some(PendingPopupAction::RenameCurrentBoard);
                self.popups.push(Popup::TextPrompt(TextPromptState {
                    prompt: "Rename current board".to_string(),
                    input: self.board.name.clone(),
                    error: None,
                }));
            }
            BOARD_MENU_DELETE => self.open_board_delete_picker(),
            BOARD_MENU_SWITCH => self.open_board_switcher(),
            BOARD_MENU_LOCK => self.lock_current_board(),
            BOARD_MENU_INFO => {
                self.message =
                    Some(format!("current board: \"{}\" [{}]", self.board.name, self.board.kind.as_str()));
            }
            _ => {}
        }
    }

    /// Opens the board-picker dropdown for the `:board` menu's "Delete
    /// board…": selecting one runs it straight through
    /// `begin_delete_board`, so the same current/only-board refusal
    /// applies.
    fn open_board_delete_picker(&mut self) {
        let boards = match self.db.list_boards() {
            Ok(b) => b,
            Err(e) => {
                self.message = Some(format!("failed to list boards: {e}"));
                return;
            }
        };
        if boards.is_empty() {
            self.message = Some("no boards available".to_string());
            return;
        }
        let items = boards
            .iter()
            .map(|b| SelectItem {
                id: b.id,
                label: if b.kind == BoardKind::Locked { format!("{} [locked]", b.name) } else { b.name.clone() },
            })
            .collect();
        self.pending_action = Some(PendingPopupAction::SelectBoardToDelete);
        self.popups.push(Popup::Dropdown(DropdownState {
            title: "Delete which board?".to_string(),
            items,
            selected: 0,
            target: DropdownTarget::BoardDelete,
        }));
    }

    fn switch_to_board(&mut self, board_id: BoardId) {
        if board_id == self.board.id {
            self.message = Some(format!("already on \"{}\"", self.board.name));
            return;
        }
        let boards = match self.db.list_boards() {
            Ok(b) => b,
            Err(e) => {
                self.message = Some(format!("failed to list boards: {e}"));
                return;
            }
        };
        let Some(target) = boards.into_iter().find(|b| b.id == board_id) else {
            self.message = Some("board no longer exists".to_string());
            return;
        };

        if self.board.kind == BoardKind::Locked && self.config.lock_on_board_switch {
            self.unlocked.remove(&self.board.id);
        }

        match target.kind {
            BoardKind::Plain => {
                self.board = target;
                self.pane = Pane::Board;
                self.focused = 0;
                self.message = Some(format!("switched to \"{}\"", self.board.name));
                let _ = self.reload();
            }
            BoardKind::Locked => {
                if self.unlocked.contains_key(&target.id) {
                    self.board = target;
                    self.pane = Pane::Board;
                    self.focused = 0;
                    self.message = Some(format!("switched to \"{}\"", self.board.name));
                    let _ = self.reload();
                } else {
                    self.pending_action = Some(PendingPopupAction::UnlockBoard(target.id));
                    self.popups.push(Popup::Passphrase(PassphraseState {
                        prompt: format!("Passphrase for \"{}\"", target.name),
                        input: String::new(),
                        error: None,
                    }));
                }
            }
        }
    }

    fn try_unlock_board(&mut self, board_id: BoardId, passphrase: String) {
        let boards = self.db.list_boards().unwrap_or_default();
        let Some(board) = boards.into_iter().find(|b| b.id == board_id) else {
            self.message = Some("board no longer exists".to_string());
            return;
        };
        let (Some(db_path), Some(salt), Some(m), Some(t), Some(p)) =
            (board.db_path.clone(), board.kdf_salt.clone(), board.kdf_m_cost, board.kdf_t_cost, board.kdf_p_cost)
        else {
            self.message = Some("board is missing its encryption metadata".to_string());
            return;
        };
        let kdf = crate::crypto::Kdf { m_cost: m, t_cost: t, p_cost: p };
        let key = match crate::crypto::derive_key(&passphrase, &salt, kdf) {
            Ok(k) => k,
            Err(e) => {
                self.message = Some(format!("key derivation failed: {e}"));
                return;
            }
        };
        let key_hex = crate::crypto::key_to_hex(&key);

        match LockedDb::open(std::path::Path::new(&db_path), &key_hex, self.config.journal_mode) {
            Ok(locked) => {
                self.unlocked.insert(board.id, locked);
                self.board = board;
                self.pane = Pane::Board;
                self.focused = 0;
                self.message = Some(format!("unlocked \"{}\"", self.board.name));
                let _ = self.reload();
            }
            Err(StorageError::WrongPassphrase) => {
                self.pending_action = Some(PendingPopupAction::UnlockBoard(board_id));
                self.popups.push(Popup::Passphrase(PassphraseState {
                    prompt: format!("Passphrase for \"{}\"", board.name),
                    input: String::new(),
                    error: Some("wrong passphrase".to_string()),
                }));
            }
            Err(e) => {
                self.message = Some(format!("failed to open board: {e}"));
            }
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::Enter => {
                self.search_active = !self.search.is_empty();
                self.mode = Mode::Normal;
            }
            KeyCode::Esc => {
                self.search.clear();
                self.search_active = false;
                self.mode = Mode::Normal;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.clear();
                self.search_active = false;
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.search.pop();
                self.clamp_selection_to_visible();
            }
            KeyCode::Char(c) => {
                self.search.push(c);
                self.clamp_selection_to_visible();
            }
            _ => {}
        }
    }

    /// Moves the selection to the next (`dir >= 0`) or previous (`dir < 0`)
    /// match for the active search query, in the focused pane, wrapping
    /// around. Reports a message instead when there is no active search or
    /// no match.
    fn jump_search(&mut self, dir: i32) {
        let Some(query) = self.search_query() else {
            self.message = Some("no active search".to_string());
            return;
        };
        match self.pane {
            Pane::Board => {
                let idx = self.focused;
                let matches: Vec<usize> = self.columns[idx]
                    .tasks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| task_matches_query(t, &query))
                    .map(|(i, _)| i)
                    .collect();
                if matches.is_empty() {
                    self.message = Some("no matches".to_string());
                    return;
                }
                let cur = self.columns[idx].selected;
                self.columns[idx].selected = advance_in(&matches, cur, dir);
            }
            Pane::Notes => {
                let matches: Vec<usize> = self
                    .notes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| note_matches_query(n, &query))
                    .map(|(i, _)| i)
                    .collect();
                if matches.is_empty() {
                    self.message = Some("no matches".to_string());
                    return;
                }
                let cur = self.notes_selected;
                self.notes_selected = advance_in(&matches, cur, dir);
            }
        }
        self.message = None;
    }

    /// Executes a resolved `Action`. Only ever called with an empty popup
    /// stack (`handle_key` routes to the stack first when one is open).
    pub fn dispatch(&mut self, action: Action) {
        match action {
            Action::Nop => {}
            Action::Quit => self.should_quit = true,
            Action::Help => self.popups.push(Popup::Help),
            Action::Cancel => {
                self.mode = Mode::Normal;
                self.cmdline.clear();
                self.search.clear();
                self.search_active = false;
                self.pending.clear();
                // Esc always returns focus to the board -- including from
                // the notes pane, which otherwise had no way back once
                // hiding the pane (`ToggleNotesPane`) stopped being the
                // only route out of it.
                self.pane = Pane::Board;
            }
            Action::Refresh => {
                self.last_external_check = Some(std::time::Instant::now());
                if !self.reconcile_external_change() {
                    self.message = match self.reload() {
                        Ok(()) => None,
                        Err(e) => Some(format!("refresh failed: {e}")),
                    };
                }
            }
            Action::MoveUp(n) => {
                self.move_selection(-(i64::from(n)));
                self.message = None;
            }
            Action::MoveDown(n) => {
                self.move_selection(i64::from(n));
                self.message = None;
            }
            Action::MoveTop => {
                self.set_selection_edge(false);
                self.message = None;
            }
            Action::MoveBottom => {
                self.set_selection_edge(true);
                self.message = None;
            }
            Action::HalfPageUp => {
                self.move_selection(-i64::from(HALF_PAGE));
                self.message = None;
            }
            Action::HalfPageDown => {
                self.move_selection(i64::from(HALF_PAGE));
                self.message = None;
            }
            Action::FocusPrevColumn => {
                self.pane = Pane::Board;
                self.focused = self.focused.saturating_sub(1);
                self.message = None;
            }
            Action::FocusNextColumn => {
                self.pane = Pane::Board;
                self.focused = (self.focused + 1).min(2);
                self.message = None;
            }
            Action::ToggleNotesPane => {
                self.show_notes = !self.show_notes;
                // Hiding the pane while it is focused must give focus back
                // to the board: otherwise keys kept going to an invisible
                // pane and the app looked frozen.
                if !self.show_notes {
                    self.pane = Pane::Board;
                }
                self.message = None;
            }
            Action::TogglePane => {
                // Only meaningful while the notes pane is visible; with it
                // hidden this is a no-op rather than focusing a pane the
                // user cannot see or reach any other way.
                if self.show_notes {
                    self.pane = match self.pane {
                        Pane::Board => Pane::Notes,
                        Pane::Notes => Pane::Board,
                    };
                }
                self.message = None;
            }
            Action::StartSearch => {
                self.mode = Mode::Search;
                self.search.clear();
            }
            Action::StartCommand => {
                self.mode = Mode::Command;
                self.cmdline.clear();
            }
            Action::SearchNext => self.jump_search(1),
            Action::SearchPrev => self.jump_search(-1),

            Action::NewTask => {
                match self.pane {
                    Pane::Board => {
                        let status = Status::ALL[self.focused];
                        self.popups.push(Popup::Form(FormState::new_task(status)));
                    }
                    Pane::Notes => {
                        self.popups.push(Popup::Form(FormState::new_note()));
                    }
                }
                self.message = None;
            }

            Action::EditSelected => match self.pane {
                Pane::Board => {
                    if let Some(task) = self.selected_task().cloned() {
                        let tags = task.tags.iter().map(|t| t.name.clone()).collect();
                        self.popups.push(Popup::Form(FormState::edit_task(
                            task.id,
                            task.title,
                            task.body,
                            task.priority,
                            task.status,
                            task.start_date,
                            task.time_expected,
                            task.deadline,
                            tags,
                        )));
                        self.message = None;
                    } else {
                        self.message = Some("no task selected".to_string());
                    }
                }
                Pane::Notes => {
                    if let Some(note) = self.selected_note().cloned() {
                        self.popups.push(Popup::Form(FormState::edit_note(note.id, &note.body)));
                        self.message = None;
                    } else {
                        self.message = Some("no note selected".to_string());
                    }
                }
            },

            Action::OpenInEditor => {
                if self.pane == Pane::Board {
                    if let Some(task) = self.selected_task().cloned() {
                        self.pending_edit = Some((EditTarget::Task(task.id), task.body));
                    } else {
                        self.message = Some("no task selected".to_string());
                    }
                } else {
                    self.message = Some("open in editor is only for tasks".to_string());
                }
            }

            Action::DeleteSelected => match self.pane {
                Pane::Board => {
                    if let Some(task) = self.selected_task() {
                        let id = task.id;
                        if self.config.confirm_delete {
                            let message = format!("delete \"{}\"?", task.title);
                            self.pending_action = Some(PendingPopupAction::ConfirmDeleteTask(id));
                            self.popups.push(Popup::Confirm(ConfirmState { message }));
                        } else {
                            self.delete_task_now(id);
                        }
                    } else {
                        self.message = Some("no task selected".to_string());
                    }
                }
                Pane::Notes => {
                    if let Some(note) = self.selected_note() {
                        let id = note.id;
                        if self.config.confirm_delete {
                            self.pending_action = Some(PendingPopupAction::ConfirmDeleteNote(id));
                            self.popups.push(Popup::Confirm(ConfirmState {
                                message: "delete this note?".to_string(),
                            }));
                        } else {
                            self.delete_note_now(id);
                        }
                    } else {
                        self.message = Some("no note selected".to_string());
                    }
                }
            },

            Action::CaptureNote => {
                // A genuine one-line quick capture: saves on Enter without
                // ever leaving the board, unlike the full New-note form
                // (still reachable via `a`/`i` while the notes pane is
                // focused).
                self.pending_action = Some(PendingPopupAction::QuickCaptureNote);
                self.popups.push(Popup::TextPrompt(TextPromptState {
                    prompt: "Quick note".to_string(),
                    input: String::new(),
                    error: None,
                }));
                self.message = None;
            }

            Action::PromoteNote => {
                if let Some(note) = self.selected_note() {
                    let id = note.id;
                    self.pending_action = Some(PendingPopupAction::PromoteNote(id));
                    self.popups.push(Popup::Dropdown(DropdownState {
                        title: "Promote to column".to_string(),
                        items: column_dropdown_items(),
                        selected: 0,
                        target: DropdownTarget::Column,
                    }));
                } else {
                    self.message = Some("no note selected".to_string());
                }
            }

            Action::OpenColumnDropdown => {
                if let Some(task) = self.selected_task() {
                    let id = task.id;
                    let selected = Status::ALL.iter().position(|s| *s == task.status).unwrap_or(0);
                    self.pending_action = Some(PendingPopupAction::MoveTaskColumn(id));
                    self.popups.push(Popup::Dropdown(DropdownState {
                        title: "Status".to_string(),
                        items: column_dropdown_items(),
                        selected,
                        target: DropdownTarget::Column,
                    }));
                } else {
                    self.message = Some("no task selected".to_string());
                }
            }

            Action::OpenPriorityDropdown => {
                if let Some(task) = self.selected_task() {
                    let id = task.id;
                    let (items, selected) = priority_dropdown_items(task.priority);
                    self.pending_action = Some(PendingPopupAction::SetPriority(id));
                    self.popups.push(Popup::Dropdown(DropdownState {
                        title: "Priority".to_string(),
                        items,
                        selected,
                        target: DropdownTarget::Priority,
                    }));
                } else {
                    self.message = Some("no task selected".to_string());
                }
            }

            Action::OpenTagDropdown => {
                if let Some(task) = self.selected_task() {
                    let id = task.id;
                    self.open_tag_picker(id);
                    self.message = None;
                } else {
                    self.message = Some("no task selected".to_string());
                }
            }

            Action::MoveTaskPrevColumn => self.move_task_column(-1),
            Action::MoveTaskNextColumn => self.move_task_column(1),
            Action::ReorderTaskUp => self.reorder_selected(-1),
            Action::ReorderTaskDown => self.reorder_selected(1),

            Action::Undo => self.do_undo(),
            Action::Redo => self.do_redo(),

            Action::OpenBoardSwitcher => self.open_board_switcher(),
            Action::OpenArchiveBrowser => {
                if self.pane == Pane::Board {
                    self.open_archive_browser();
                } else {
                    self.message = Some("archive is only for the board".to_string());
                }
            }
        }
    }

    /// Moves the selection `delta` steps, skipping any row hidden by an
    /// active search filter rather than landing on it. `columns[idx].tasks`
    /// and `.selected` (and `notes`/`notes_selected`) stay the single
    /// source of truth throughout -- only the *walk* is filter-aware.
    fn move_selection(&mut self, delta: i64) {
        match self.pane {
            Pane::Board => {
                let idx = self.focused;
                let visible = self.visible_task_indices(idx);
                if let Some(new) = step_visible(&visible, self.columns[idx].selected, delta) {
                    self.columns[idx].selected = new;
                }
            }
            Pane::Notes => {
                let visible = self.visible_note_indices();
                if let Some(new) = step_visible(&visible, self.notes_selected, delta) {
                    self.notes_selected = new;
                }
            }
        }
    }

    /// Jumps the selection to the first (`last == false`) or last
    /// (`last == true`) visible row -- `gg`/`G`. A no-op when nothing is
    /// visible, same as `move_selection`.
    fn set_selection_edge(&mut self, last: bool) {
        match self.pane {
            Pane::Board => {
                let idx = self.focused;
                let visible = self.visible_task_indices(idx);
                let target = if last { visible.last() } else { visible.first() };
                if let Some(&t) = target {
                    self.columns[idx].selected = t;
                }
            }
            Pane::Notes => {
                let visible = self.visible_note_indices();
                let target = if last { visible.last() } else { visible.first() };
                if let Some(&t) = target {
                    self.notes_selected = t;
                }
            }
        }
    }

    /// Raw indices, ascending, of tasks in column `idx` visible under the
    /// active search filter -- every index when no filter is active.
    fn visible_task_indices(&self, idx: usize) -> Vec<usize> {
        let tasks = &self.columns[idx].tasks;
        match self.search_query() {
            Some(q) => tasks.iter().enumerate().filter(|(_, t)| task_matches_query(t, &q)).map(|(i, _)| i).collect(),
            None => (0..tasks.len()).collect(),
        }
    }

    /// Same as `visible_task_indices`, for the notes list.
    fn visible_note_indices(&self) -> Vec<usize> {
        match self.search_query() {
            Some(q) => self.notes.iter().enumerate().filter(|(_, n)| note_matches_query(n, &q)).map(|(i, _)| i).collect(),
            None => (0..self.notes.len()).collect(),
        }
    }

    /// Restores "selection points at a visible row" after the search
    /// filter or the underlying data changes -- called after every reload
    /// and on each character typed into a live search, so a hidden row is
    /// never left selected long enough for a subsequent action to reach
    /// it. A column (or the notes list) with no visible rows at all is
    /// left untouched: there is no visible row to land on, and
    /// `selected_task`/`selected_note` already treat a selection hidden
    /// this way as "nothing selected".
    fn clamp_selection_to_visible(&mut self) {
        for idx in 0..self.columns.len() {
            let visible = self.visible_task_indices(idx);
            if !visible.is_empty() && !visible.contains(&self.columns[idx].selected) {
                self.columns[idx].selected = visible[0];
            }
        }
        let visible_notes = self.visible_note_indices();
        if !visible_notes.is_empty() && !visible_notes.contains(&self.notes_selected) {
            self.notes_selected = visible_notes[0];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::JournalMode;
    use crate::domain::task::{NewTask as DomainNewTask, Priority as DomainPriority};

    fn test_app_with_tasks() -> App {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("test", BoardKind::Plain).unwrap();
        {
            let store = db.store_for(board_id);
            for title in ["a", "b", "c"] {
                store
                    .create_task(DomainNewTask {
                        title: title.to_string(),
                        body: String::new(),
                        status: Status::ToDo,
                        priority: DomainPriority::Normal,
                    start_date: None,
                    time_expected: None,
                    deadline: None,
                    })
                    .unwrap();
            }
        }
        let board = db.get_board_by_name("test").unwrap().unwrap();
        App::new(Config::default(), db, board).unwrap()
    }

    #[test]
    fn deadline_notifications_are_recorded_once() {
        let mut app = test_app_with_tasks();
        let id = app
            .store()
            .create_task(DomainNewTask {
                title: "overdue".to_string(),
                body: String::new(),
                status: Status::ToDo,
                priority: DomainPriority::Normal,
                start_date: None,
                time_expected: None,
                deadline: Some(chrono::Utc::now().timestamp() - 1),
            })
            .unwrap();
        app.reload().unwrap();

        assert_eq!(app.unnotified_deadline_tasks().iter().map(|task| task.id).collect::<Vec<_>>(), vec![id]);
        app.mark_deadline_notified(id).unwrap();
        assert!(app.unnotified_deadline_tasks().is_empty());
        assert!(app.store().get_task(id).unwrap().deadline_notified_at.is_some());
    }

    /// Same shape as `test_app_with_tasks`, but backed by a real on-disk
    /// `main.db` under `dir` (`journal_mode = Delete`, so every write lands
    /// in the single file and its fingerprint reliably changes -- see
    /// `App::record_fingerprints`), for the external-change-detection tests
    /// below. Seeds one task titled "original".
    fn test_app_on_disk(dir: &tempfile::TempDir) -> App {
        let path = dir.path().join("main.db");
        let db = MainDb::open(&path, JournalMode::Delete).unwrap();
        let board_id = db.create_board("test", BoardKind::Plain).unwrap();
        db.store_for(board_id)
            .create_task(DomainNewTask {
                title: "original".to_string(),
                body: String::new(),
                status: Status::ToDo,
                priority: DomainPriority::Normal,
                start_date: None,
                time_expected: None, deadline: None,
            })
            .unwrap();
        let board = db.get_board_by_name("test").unwrap().unwrap();
        let mut app = App::new(Config::default(), db, board).unwrap();
        app.set_data_dir(dir.path().to_path_buf());
        app
    }

    // --- external database change detection --------------------------------

    #[test]
    fn external_change_reloads_from_replaced_main_db() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app_on_disk(&dir);
        assert_eq!(app.columns[0].tasks[0].title, "original");

        let other_path = dir.path().join("other.db");
        {
            let other = MainDb::open(&other_path, JournalMode::Delete).unwrap();
            let board_id = other.create_board("test", BoardKind::Plain).unwrap();
            other
                .store_for(board_id)
                .create_task(DomainNewTask {
                    title: "from the other machine".to_string(),
                    body: String::new(),
                    status: Status::ToDo,
                    priority: DomainPriority::Normal,
                    start_date: None,
                    time_expected: None, deadline: None,
                })
                .unwrap();
        }
        std::fs::rename(&other_path, dir.path().join("main.db")).unwrap();

        let changed = app.reconcile_external_change();

        assert!(changed);
        assert_eq!(app.columns[0].tasks.len(), 1);
        assert_eq!(app.columns[0].tasks[0].title, "from the other machine");
        assert_eq!(app.undo.undo_len(), 0);
        assert_eq!(app.undo.redo_len(), 0);
        assert_eq!(app.message.as_deref(), Some("reloaded: database changed on disk"));
    }

    #[test]
    fn write_after_external_reload_lands_in_the_new_file() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut app = test_app_on_disk(&dir);

            let other_path = dir.path().join("other.db");
            {
                let other = MainDb::open(&other_path, JournalMode::Delete).unwrap();
                other.create_board("test", BoardKind::Plain).unwrap();
            }
            std::fs::rename(&other_path, dir.path().join("main.db")).unwrap();
            assert!(app.reconcile_external_change());

            app.dispatch(Action::NewTask);
            if let Some(Popup::Form(f)) = app.popups.last_mut() {
                f.title = "written after reload".to_string();
            }
            let outcome = app
                .popups
                .last_mut()
                .unwrap()
                .handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
            app.apply_popup_outcome(outcome);
            // `app` (and its `MainDb` connection) is dropped at the end of
            // this block, before a second connection reopens the same file
            // below -- two live connections to one DELETE-journal-mode
            // file can otherwise race into `SQLITE_BUSY`.
        }

        let reread = MainDb::open(&dir.path().join("main.db"), JournalMode::Delete).unwrap();
        let board = reread.get_board_by_name("test").unwrap().unwrap();
        let tasks = reread.store_for(board.id).list_tasks(Status::ToDo).unwrap();
        assert!(tasks.iter().any(|t| t.title == "written after reload"));
    }

    #[test]
    fn own_writes_do_not_trigger_a_reload_message() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app_on_disk(&dir);

        app.dispatch(Action::NewTask);
        if let Some(Popup::Form(f)) = app.popups.last_mut() {
            f.title = "own write".to_string();
        }
        let outcome = app
            .popups
            .last_mut()
            .unwrap()
            .handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        app.apply_popup_outcome(outcome);

        assert!(!app.reconcile_external_change());
        assert_ne!(app.message.as_deref(), Some("reloaded: database changed on disk"));
    }

    #[test]
    fn missing_main_db_during_check_does_not_panic_and_keeps_old_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app_on_disk(&dir);
        std::fs::remove_file(dir.path().join("main.db")).unwrap();

        let changed = app.reconcile_external_change();

        assert!(!changed);
        assert_eq!(app.columns[0].tasks.len(), 1);
        assert_eq!(app.columns[0].tasks[0].title, "original");
    }

    /// Drives `App` through its own `:board new <name> locked` command
    /// flow (typing the passphrase twice and confirming the
    /// unrecoverable-data warning), so tests exercise the real pipeline
    /// rather than poking storage directly. `app`'s data dir is pointed at
    /// `dir` first, so the encrypted file lands somewhere disposable.
    fn create_locked_board_for_test(app: &mut App, dir: &tempfile::TempDir, name: &str, passphrase: &str) {
        app.set_data_dir(dir.path().to_path_buf());
        app.mode = Mode::Command;
        for c in format!("board new {name} locked").chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::Passphrase(_))), "expected first passphrase prompt");
        for c in passphrase.chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::Passphrase(_))), "expected confirm passphrase prompt");
        for c in passphrase.chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::Confirm(_))), "expected unrecoverable-data confirm");
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.popups.is_empty());
    }

    /// Regression: `o` must only *record* an edit request. An earlier
    /// version ran the editor from inside `App` via a second `Terminal`,
    /// whose `clear()` failed with "the cursor position could not be
    /// read" and silently threw away whatever the user had written.
    #[test]
    fn open_in_editor_records_a_request_instead_of_running_an_editor() {
        let mut app = test_app_with_tasks();
        let id = app.columns[0].tasks[0].id;
        app.dispatch(Action::OpenInEditor);
        assert_eq!(app.take_pending_edit(), Some((EditTarget::Task(id), String::new())));
        // taken exactly once
        assert_eq!(app.take_pending_edit(), None);
    }

    /// Regression: text coming back from the editor must reach storage.
    #[test]
    fn apply_edited_text_persists_the_task_body() {
        let mut app = test_app_with_tasks();
        let id = app.columns[0].tasks[0].id;
        app.apply_edited_text(EditTarget::Task(id), "edited body\nline two".to_string());

        let stored = app.db.store_for(app.board.id).get_task(id).unwrap();
        assert_eq!(stored.body, "edited body\nline two");
        assert_eq!(app.columns[0].tasks[0].body, "edited body\nline two", "reload must refresh the cache");
    }

    #[test]
    fn apply_edited_text_updates_an_open_form_body() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::NewTask);
        app.apply_edited_text(EditTarget::FormBody, "from the editor".to_string());
        match app.popups.last() {
            Some(Popup::Form(f)) => assert_eq!(f.body, "from the editor"),
            other => panic!("expected a form on the stack, got {other:?}"),
        }
    }

    #[test]
    fn editor_command_prefers_nvim_over_the_environment() {
        // Guards the standing requirement that $EDITOR/$VISUAL never
        // outrank nvim when nvim is installed.
        let app = test_app_with_tasks();
        if crate::config::path_lookup("nvim") {
            assert_eq!(app.editor_command().unwrap(), "nvim");
        }
    }

    #[test]
    fn new_app_loads_tasks() {
        let app = test_app_with_tasks();
        assert_eq!(app.columns[0].tasks.len(), 3);
        assert_eq!(app.columns[1].tasks.len(), 0);
        assert_eq!(app.columns[2].tasks.len(), 0);
    }

    #[test]
    fn move_down_and_up_clamp() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::MoveDown(1));
        assert_eq!(app.columns[0].selected, 1);
        app.dispatch(Action::MoveDown(10));
        assert_eq!(app.columns[0].selected, 2);
        app.dispatch(Action::MoveUp(10));
        assert_eq!(app.columns[0].selected, 0);
    }

    #[test]
    fn move_top_and_bottom() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::MoveBottom);
        assert_eq!(app.columns[0].selected, 2);
        app.dispatch(Action::MoveTop);
        assert_eq!(app.columns[0].selected, 0);
    }

    #[test]
    fn focus_column_clamps_at_edges() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::FocusPrevColumn);
        assert_eq!(app.focused, 0);
        app.dispatch(Action::FocusNextColumn);
        app.dispatch(Action::FocusNextColumn);
        app.dispatch(Action::FocusNextColumn);
        assert_eq!(app.focused, 2);
    }

    #[test]
    fn toggle_notes_pane_and_switch_pane() {
        let mut app = test_app_with_tasks();
        assert!(!app.show_notes);
        app.dispatch(Action::ToggleNotesPane);
        assert!(app.show_notes);
        assert_eq!(app.pane, Pane::Board);
        app.dispatch(Action::TogglePane);
        assert_eq!(app.pane, Pane::Notes);
    }

    /// Regression: hiding the notes pane while it is focused must return
    /// focus to the board -- otherwise keys kept going to an invisible
    /// pane and the app looked frozen.
    #[test]
    fn hiding_the_notes_pane_while_focused_restores_board_focus() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::ToggleNotesPane); // show
        app.dispatch(Action::TogglePane); // focus -> Notes
        assert_eq!(app.pane, Pane::Notes);

        app.dispatch(Action::ToggleNotesPane); // hide
        assert!(!app.show_notes);
        assert_eq!(app.pane, Pane::Board, "hiding the pane must restore board focus");
    }

    #[test]
    fn tab_is_a_noop_while_the_notes_pane_is_hidden() {
        let mut app = test_app_with_tasks();
        assert!(!app.show_notes);
        app.dispatch(Action::TogglePane);
        assert_eq!(app.pane, Pane::Board, "Tab must not focus an invisible pane");
    }

    #[test]
    fn esc_in_the_notes_pane_returns_focus_to_the_board() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::ToggleNotesPane);
        app.dispatch(Action::TogglePane);
        assert_eq!(app.pane, Pane::Notes);

        app.dispatch(Action::Cancel);
        assert_eq!(app.pane, Pane::Board);
        assert!(app.show_notes, "Esc only moves focus, it does not hide the pane");
    }

    #[test]
    fn help_action_pushes_help_popup_which_closes_on_any_key() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app_with_tasks();
        app.dispatch(Action::Help);
        assert!(matches!(app.popups.last(), Some(Popup::Help)));
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.popups.is_empty());
    }

    #[test]
    fn command_mode_quit_via_colon_q() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app_with_tasks();
        app.mode = Mode::Command;
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.should_quit);
    }

    #[test]
    fn bootstrap_creates_default_board_when_empty() {
        let db = MainDb::open_in_memory().unwrap();
        let board = bootstrap_and_select_board(&db, None, None).unwrap();
        assert_eq!(board.name, "personal");
    }

    #[test]
    fn bootstrap_errors_on_unknown_requested_board() {
        let db = MainDb::open_in_memory().unwrap();
        let err = bootstrap_and_select_board(&db, Some("nope"), None).unwrap_err();
        assert!(matches!(err, AppError::Config(_)));
    }

    #[test]
    fn ensure_startable_rejects_locked_board_at_launch() {
        let db = MainDb::open_in_memory().unwrap();
        let id = db.create_board("vault", BoardKind::Locked).unwrap();
        let board = db.list_boards().unwrap().into_iter().find(|b| b.id == id).unwrap();
        assert!(ensure_startable(&board).is_err());
    }

    #[test]
    fn ensure_startable_allows_plain_board() {
        let db = MainDb::open_in_memory().unwrap();
        db.create_board("alpha", BoardKind::Plain).unwrap();
        let board = db.get_board_by_name("alpha").unwrap().unwrap();
        assert!(ensure_startable(&board).is_ok());
    }

    // --- Phase 5: task/note editing --------------------------------------

    #[test]
    fn new_task_opens_a_form_for_the_focused_column() {
        let mut app = test_app_with_tasks();
        app.focused = 1;
        app.dispatch(Action::NewTask);
        match app.popups.last() {
            Some(Popup::Form(f)) => assert_eq!(f.kind, FormKind::NewTask(Status::Doing)),
            other => panic!("expected a new-task form, got {other:?}"),
        }
    }

    #[test]
    fn submitting_a_new_task_creates_it_and_selects_it() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::NewTask);
        if let Some(Popup::Form(f)) = app.popups.last_mut() {
            f.title = "write the plan".to_string();
        }
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        ));
        app.apply_popup_outcome(outcome);
        assert!(app.popups.is_empty());
        assert_eq!(app.columns[0].tasks.len(), 4);
        let selected = &app.columns[0].tasks[app.columns[0].selected];
        assert_eq!(selected.title, "write the plan");
        assert_eq!(app.undo.undo_len(), 1);
    }

    #[test]
    fn empty_title_is_rejected_and_form_stays_open() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::NewTask);
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        ));
        app.apply_popup_outcome(outcome);
        assert!(!app.popups.is_empty());
        assert_eq!(app.columns[0].tasks.len(), 3);
    }

    #[test]
    fn delete_with_confirm_then_undo_restores_the_task() {
        let mut app = test_app_with_tasks();
        let title_before = app.columns[0].tasks[0].title.clone();
        app.dispatch(Action::DeleteSelected);
        assert!(matches!(app.popups.last(), Some(Popup::Confirm(_))));
        let outcome = app
            .popups
            .last_mut()
            .unwrap()
            .handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);
        assert_eq!(app.columns[0].tasks.len(), 2);

        app.dispatch(Action::Undo);
        assert_eq!(app.columns[0].tasks.len(), 3);
        assert!(app.columns[0].tasks.iter().any(|t| t.title == title_before));
    }

    #[test]
    fn delete_without_confirm_delete_config_skips_the_prompt() {
        let mut app = test_app_with_tasks();
        app.config.confirm_delete = false;
        app.dispatch(Action::DeleteSelected);
        assert!(app.popups.is_empty());
        assert_eq!(app.columns[0].tasks.len(), 2);
    }

    #[test]
    fn move_task_next_column_keeps_both_columns_dense() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::MoveTaskNextColumn);
        assert_eq!(app.columns[0].tasks.len(), 2);
        assert_eq!(app.columns[1].tasks.len(), 1);
        let positions: Vec<i64> = app.columns[0].tasks.iter().map(|t| t.position).collect();
        assert_eq!(positions, vec![0, 1]);
    }

    #[test]
    fn priority_dropdown_updates_selected_task() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::OpenPriorityDropdown);
        if let Some(Popup::Dropdown(d)) = app.popups.last_mut() {
            let idx = d.items.iter().position(|i| i.label == "urgent").unwrap();
            d.selected = idx;
        }
        let outcome = app
            .popups
            .last_mut()
            .unwrap()
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);
        assert!(app.popups.is_empty());
        assert_eq!(app.columns[0].tasks[0].priority, Priority::Urgent);
    }

    #[test]
    fn reorder_up_and_down_swap_neighbours() {
        let mut app = test_app_with_tasks();
        let b_id = app.columns[0].tasks[1].id;
        app.columns[0].selected = 1;
        app.dispatch(Action::ReorderTaskUp);
        assert_eq!(app.columns[0].tasks[0].id, b_id);
    }

    #[test]
    fn undo_redo_round_trip_on_create() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::NewTask);
        if let Some(Popup::Form(f)) = app.popups.last_mut() {
            f.title = "temp".to_string();
        }
        let outcome = app
            .popups
            .last_mut()
            .unwrap()
            .handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        app.apply_popup_outcome(outcome);
        assert_eq!(app.columns[0].tasks.len(), 4);

        app.dispatch(Action::Undo);
        assert_eq!(app.columns[0].tasks.len(), 3);

        app.dispatch(Action::Redo);
        assert_eq!(app.columns[0].tasks.len(), 4);
        assert!(app.columns[0].tasks.iter().any(|t| t.title == "temp"));
    }

    #[test]
    fn undo_on_empty_stack_reports_nothing_to_undo() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::Undo);
        assert_eq!(app.message.as_deref(), Some("nothing to undo"));
    }

    // --- Phase: Status field / S key / date fields ------------------------

    #[test]
    fn shift_s_opens_a_status_dropdown_and_moves_the_task() {
        let mut app = test_app_with_tasks();
        let id = app.columns[0].tasks[0].id;
        app.handle_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        assert!(matches!(app.popups.last(), Some(Popup::Dropdown(_))));
        if let Some(Popup::Dropdown(d)) = app.popups.last_mut() {
            let idx = d.items.iter().position(|i| i.label == "Done").unwrap();
            d.selected = idx;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.popups.is_empty());
        assert_eq!(app.columns[2].tasks.iter().find(|t| t.id == id).unwrap().status, Status::Done);
        assert!(app.columns[0].tasks.iter().all(|t| t.id != id));
    }

    #[test]
    fn edit_task_form_carries_status_and_dates_and_round_trips() {
        let mut app = test_app_with_tasks();
        let id = app.columns[0].tasks[0].id;
        {
            let store = app.db.store_for(app.board.id);
            store
                .update_task(
                    id,
                    TaskPatch { deadline: Some(Some(1_735_000_000)), ..Default::default() },
                )
                .unwrap();
        }
        let _ = app.reload();

        app.dispatch(Action::EditSelected);
        let Some(Popup::Form(f)) = app.popups.last() else { panic!("expected form") };
        assert_eq!(f.deadline_text, crate::domain::dates::format_date(1_735_000_000));

        // Change status via the form's Status field and save.
        let mut outcome;
        {
            let f = match app.popups.last_mut() { Some(Popup::Form(f)) => f, _ => unreachable!() };
            f.field = Field::Status;
            outcome = f.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
        app.apply_popup_outcome(outcome);
        if let Some(Popup::Dropdown(d)) = app.popups.last_mut() {
            let idx = d.items.iter().position(|i| i.label == "Doing").unwrap();
            d.selected = idx;
        }
        outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);

        outcome = app
            .popups
            .last_mut()
            .unwrap()
            .handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        app.apply_popup_outcome(outcome);
        assert!(app.popups.is_empty());

        let moved = app.columns[1].tasks.iter().find(|t| t.id == id).expect("task moved to Doing");
        // The deadline round-trips through the form's text field at
        // day granularity (`dates::format_date`/`parse_date`), so it is
        // normalized to that day's local midnight rather than preserving
        // the original sub-day timestamp exactly.
        assert_eq!(
            crate::domain::dates::format_date(moved.deadline.unwrap()),
            crate::domain::dates::format_date(1_735_000_000)
        );
    }

    // --- Phase: tag create/rename/delete/toggle ----------------------------

    #[test]
    fn tag_new_toggle_rename_delete_round_trip_and_persist() {
        let mut app = test_app_with_tasks();
        let id = app.columns[0].tasks[0].id;

        // `t` opens the tag picker.
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::TagPicker(_))));

        // Enter on "+ new tag..." starts the new-tag flow.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::TextPrompt(_))));
        for c in "urgent-home".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.popups.is_empty());

        let tags = app.store().tags_for_task(id).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "urgent-home");
        assert!(app.columns[0].tasks.iter().find(|t| t.id == id).unwrap().tags.iter().any(|t| t.name == "urgent-home"));

        // Toggle it back off from the picker.
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // toggle off the first (only) tag row
        assert!(matches!(app.popups.last(), Some(Popup::TagPicker(_))), "toggle keeps the picker open");
        assert!(app.store().tags_for_task(id).unwrap().is_empty());

        // Toggle it back on, then rename it.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.store().tags_for_task(id).unwrap().len(), 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::TextPrompt(_))));
        // Clear the prefilled name and type a new one.
        for _ in 0.."urgent-home".len() {
            app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        for c in "renamed".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.popups.is_empty());
        let tags = app.store().list_tags().unwrap();
        assert_eq!(tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["renamed"]);

        // Delete it (with confirm).
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::Confirm(_))));
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.popups.is_empty());
        assert!(app.store().list_tags().unwrap().is_empty());
    }

    #[test]
    fn empty_new_tag_name_is_rejected() {
        let mut app = test_app_with_tasks();
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // "+ new tag..."
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // submit empty
        assert_eq!(app.message.as_deref(), Some("tag name must not be empty"));
    }

    // --- Phase: a/e/dd act on the focused pane ------------------------------

    #[test]
    fn a_in_notes_pane_opens_a_new_note_form_not_a_new_task_form() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::ToggleNotesPane);
        app.dispatch(Action::TogglePane);
        assert_eq!(app.pane, Pane::Notes);

        app.dispatch(Action::NewTask); // bound to 'a'/'i'
        match app.popups.last() {
            Some(Popup::Form(f)) => assert_eq!(f.kind, FormKind::NewNote),
            other => panic!("expected a new-note form, got {other:?}"),
        }
    }

    #[test]
    fn e_in_notes_pane_edits_the_selected_note() {
        let mut app = test_app_with_tasks();
        app.store().create_note("my note").unwrap();
        let _ = app.reload();
        app.dispatch(Action::ToggleNotesPane);
        app.dispatch(Action::TogglePane);

        app.dispatch(Action::EditSelected);
        match app.popups.last() {
            Some(Popup::Form(f)) => assert_eq!(f.kind, FormKind::EditNote),
            other => panic!("expected an edit-note form, got {other:?}"),
        }
    }

    #[test]
    fn dd_in_notes_pane_deletes_the_selected_note() {
        let mut app = test_app_with_tasks();
        app.store().create_note("throwaway").unwrap();
        let _ = app.reload();
        app.dispatch(Action::ToggleNotesPane);
        app.dispatch(Action::TogglePane);
        app.config.confirm_delete = false;

        assert_eq!(app.notes.len(), 1);
        app.dispatch(Action::DeleteSelected);
        assert_eq!(app.notes.len(), 0);
    }

    #[test]
    fn gp_promote_still_works() {
        let mut app = test_app_with_tasks();
        app.store().create_note("promote me").unwrap();
        let _ = app.reload();
        app.dispatch(Action::ToggleNotesPane);
        app.dispatch(Action::TogglePane);

        app.dispatch(Action::PromoteNote);
        assert!(matches!(app.popups.last(), Some(Popup::Dropdown(_))));
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);
        assert!(app.popups.is_empty());
        assert!(app.columns[0].tasks.iter().any(|t| t.title.contains("promote me")));
    }

    // --- Phase: quick capture ('c') -----------------------------------------

    #[test]
    fn c_is_a_one_line_quick_capture_that_saves_on_enter() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::CaptureNote);
        assert!(matches!(app.popups.last(), Some(Popup::TextPrompt(_))));
        for c in "call the dentist".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.popups.is_empty());
        assert_eq!(app.notes.len(), 1);
        assert_eq!(app.notes[0].body, "call the dentist");
    }

    // --- Phase: search -------------------------------------------------------

    #[test]
    fn search_active_filters_and_n_moves_to_next_match() {
        let mut app = test_app_with_tasks(); // tasks: "a", "b", "c"
        app.store().create_task(DomainNewTask {
            title: "abc".to_string(),
            body: String::new(),
            status: Status::ToDo,
            priority: DomainPriority::Normal,
            start_date: None,
            time_expected: None, deadline: None,
        }).unwrap();
        let _ = app.reload();
        // titles now: a, b, c, abc -- "a" matches "a" (idx 0) and "abc" (idx 3)

        app.dispatch(Action::StartSearch);
        for c in "a".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // commit
        assert!(app.search_active);
        assert_eq!(app.search_query().as_deref(), Some("a"));

        app.columns[0].selected = 0;
        app.dispatch(Action::SearchNext);
        assert_eq!(app.columns[0].selected, 3);
        app.dispatch(Action::SearchNext);
        assert_eq!(app.columns[0].selected, 0, "wraps back to the first match");
        app.dispatch(Action::SearchPrev);
        assert_eq!(app.columns[0].selected, 3);
    }

    #[test]
    fn esc_in_normal_mode_clears_an_active_search_filter() {
        let mut app = test_app_with_tasks();
        app.search = "b".to_string();
        app.search_active = true;
        assert!(app.search_query().is_some());

        app.dispatch(Action::Cancel);
        assert!(app.search_query().is_none());
        assert!(!app.search_active);
        assert!(app.search.is_empty());
    }

    #[test]
    fn nohl_command_clears_an_active_search_filter() {
        let mut app = test_app_with_tasks();
        app.search = "b".to_string();
        app.search_active = true;
        app.mode = Mode::Command;
        for c in "nohl".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.search_query().is_none());
    }

    #[test]
    fn search_next_with_no_active_search_reports_a_message() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::SearchNext);
        assert_eq!(app.message.as_deref(), Some("no active search"));
    }

    // --- search: movement must skip hidden rows -----------------------------

    fn search_test_app_with_titles(titles: &[&str]) -> App {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("test", BoardKind::Plain).unwrap();
        {
            let store = db.store_for(board_id);
            for title in titles {
                store
                    .create_task(DomainNewTask {
                        title: title.to_string(),
                        body: String::new(),
                        status: Status::ToDo,
                        priority: DomainPriority::Normal,
                        start_date: None,
                        time_expected: None, deadline: None,
                    })
                    .unwrap();
            }
        }
        let board = db.get_board_by_name("test").unwrap().unwrap();
        App::new(Config::default(), db, board).unwrap()
    }

    /// Drives `/`, types `query`, and presses Enter to commit it -- the real
    /// key path a live search takes, so these regression tests exercise the
    /// same per-keystroke clamping normal play does.
    fn start_committed_search(app: &mut App, query: &str) {
        app.dispatch(Action::StartSearch);
        for c in query.chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }

    #[test]
    fn j_k_skip_hidden_rows_while_search_is_active() {
        // "cat", "dog", "cow" -- search "c" hides "dog" (raw index 1).
        let mut app = search_test_app_with_titles(&["cat", "dog", "cow"]);
        start_committed_search(&mut app, "c");
        assert_eq!(app.columns[0].tasks[app.columns[0].selected].title, "cat");

        app.dispatch(Action::MoveDown(1));
        assert_eq!(app.columns[0].tasks[app.columns[0].selected].title, "cow", "j must skip hidden \"dog\"");

        app.dispatch(Action::MoveUp(1));
        assert_eq!(app.columns[0].tasks[app.columns[0].selected].title, "cat", "k must skip hidden \"dog\" going back");
    }

    #[test]
    fn move_down_with_count_clamps_to_last_visible_row() {
        // "cat", "dog", "cow", "car" -- search "c" hides only "dog".
        let mut app = search_test_app_with_titles(&["cat", "dog", "cow", "car"]);
        start_committed_search(&mut app, "c");
        app.dispatch(Action::MoveDown(5)); // 5j: more than the 3 visible rows
        assert_eq!(app.columns[0].tasks[app.columns[0].selected].title, "car", "must clamp to the last visible row");
    }

    #[test]
    fn gg_and_shift_g_land_on_first_and_last_visible_row() {
        let mut app = search_test_app_with_titles(&["cat", "dog", "cow"]);
        start_committed_search(&mut app, "c");
        app.dispatch(Action::MoveBottom);
        assert_eq!(app.columns[0].tasks[app.columns[0].selected].title, "cow");
        app.dispatch(Action::MoveTop);
        assert_eq!(app.columns[0].tasks[app.columns[0].selected].title, "cat");
    }

    #[test]
    fn half_page_scroll_skips_hidden_rows() {
        // 6 raw rows, 5 of which match "c"; HALF_PAGE == 5 should land
        // exactly on the last visible match, not 5 raw rows down.
        let mut app = search_test_app_with_titles(&["c0", "c1", "xx", "c2", "c3", "c4"]);
        start_committed_search(&mut app, "c");
        app.dispatch(Action::HalfPageDown);
        assert_eq!(app.columns[0].tasks[app.columns[0].selected].title, "c4");
    }

    #[test]
    fn h_l_column_switch_lands_on_a_visible_row_in_the_new_column() {
        let mut app = search_test_app_with_titles(&["cat"]);
        {
            let store = app.db.store_for(app.board.id);
            for title in ["dog", "cow"] {
                store
                    .create_task(DomainNewTask {
                        title: title.to_string(),
                        body: String::new(),
                        status: Status::Doing,
                        priority: DomainPriority::Normal,
                        start_date: None,
                        time_expected: None, deadline: None,
                    })
                    .unwrap();
            }
        }
        app.reload().unwrap();
        app.columns[1].selected = 0; // "dog" -- about to be hidden by the filter
        start_committed_search(&mut app, "c");
        app.dispatch(Action::FocusNextColumn); // ToDo -> Doing
        assert_eq!(app.focused, 1);
        assert_eq!(app.columns[1].tasks[app.columns[1].selected].title, "cow", "must not land on hidden \"dog\"");
    }

    #[test]
    fn column_with_no_matches_leaves_no_task_selected() {
        let mut app = search_test_app_with_titles(&["apple", "banana"]);
        start_committed_search(&mut app, "zzz");
        assert!(app.search_active);
        app.dispatch(Action::EditSelected);
        assert!(app.popups.is_empty(), "must not open an edit form for a hidden/nonexistent row");
        assert_eq!(app.message.as_deref(), Some("no task selected"));
    }

    #[test]
    fn entering_search_hides_the_current_selection_and_snaps_it() {
        let mut app = search_test_app_with_titles(&["apple", "banana", "cherry"]);
        app.columns[0].selected = 1; // "banana"
        app.dispatch(Action::StartSearch);
        for c in "cherry".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        // Must already be off the now-hidden "banana" while still typing,
        // before Enter ever commits the filter.
        assert_eq!(app.columns[0].tasks[app.columns[0].selected].title, "cherry");
    }

    #[test]
    fn clearing_search_leaves_a_valid_selection() {
        let mut app = search_test_app_with_titles(&["apple", "banana", "cherry"]);
        app.columns[0].selected = 1;
        start_committed_search(&mut app, "cherry");
        assert_eq!(app.columns[0].tasks[app.columns[0].selected].title, "cherry");

        app.dispatch(Action::Cancel); // Esc clears the filter
        assert!(app.search_query().is_none());
        assert!(app.columns[0].selected < app.columns[0].tasks.len(), "selection must stay in bounds");
    }

    #[test]
    fn search_next_after_plain_movement_does_not_double_move() {
        let mut app = search_test_app_with_titles(&["cat", "dog", "cow", "cup"]);
        start_committed_search(&mut app, "c");
        app.dispatch(Action::MoveDown(1)); // cat -> cow, skipping hidden "dog"
        assert_eq!(app.columns[0].tasks[app.columns[0].selected].title, "cow");
        app.dispatch(Action::SearchNext); // next match after "cow" is "cup"
        assert_eq!(app.columns[0].tasks[app.columns[0].selected].title, "cup");
    }

    #[test]
    fn reorder_skips_a_hidden_neighbour() {
        let mut app = search_test_app_with_titles(&["cat", "dog", "cow"]);
        start_committed_search(&mut app, "c"); // hides "dog"
        app.dispatch(Action::ReorderTaskDown); // J: swap with the next VISIBLE row, "cow"
        assert_eq!(app.columns[0].tasks[0].title, "cow");
        assert_eq!(app.columns[0].tasks[1].title, "dog", "hidden \"dog\" must stay put");
        assert_eq!(app.columns[0].tasks[2].title, "cat");
        assert_eq!(app.columns[0].tasks[app.columns[0].selected].title, "cat", "selection follows the moved task");
    }

    #[test]
    fn reorder_at_the_visible_edge_reports_message() {
        let mut app = search_test_app_with_titles(&["cat", "dog", "cow"]);
        start_committed_search(&mut app, "c"); // "cat" is already first among the visible rows
        app.dispatch(Action::ReorderTaskUp);
        assert_eq!(app.message.as_deref(), Some("already at the end"));
    }

    #[test]
    fn delete_ignores_a_hidden_selection() {
        let mut app = search_test_app_with_titles(&["apple", "banana"]);
        start_committed_search(&mut app, "apple"); // hides "banana", selection snaps to "apple"
        app.columns[0].selected = 1; // force it back onto the now-hidden "banana"
        app.dispatch(Action::DeleteSelected);
        assert_eq!(app.columns[0].tasks.len(), 2, "must not delete a hidden row");
        assert_eq!(app.message.as_deref(), Some("no task selected"));
    }

    // --- board switching + locked boards -----------------------------------

    #[test]
    fn board_switcher_lists_boards_and_switching_to_plain_changes_tasks() {
        let mut app = test_app_with_tasks();
        app.db.create_board("other", BoardKind::Plain).unwrap();
        app.dispatch(Action::OpenBoardSwitcher);
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label == "other").unwrap();
            d.selected = idx;
        }
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);
        assert!(app.popups.is_empty());
        assert_eq!(app.board.name, "other");
        assert_eq!(app.columns[0].tasks.len(), 0);
    }

    #[test]
    fn selecting_a_locked_board_pushes_passphrase_popup() {
        let mut app = test_app_with_tasks();
        let dir = tempfile::tempdir().unwrap();
        create_locked_board_for_test(&mut app, &dir, "vault", "hunter2");

        app.dispatch(Action::OpenBoardSwitcher);
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label.contains("vault")).unwrap();
            d.selected = idx;
        }
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);
        assert!(matches!(app.popups.last(), Some(Popup::Passphrase(_))));
    }

    #[test]
    fn wrong_passphrase_keeps_popup_open_with_message_then_right_one_switches() {
        let mut app = test_app_with_tasks();
        let dir = tempfile::tempdir().unwrap();
        create_locked_board_for_test(&mut app, &dir, "vault", "hunter2");

        app.dispatch(Action::OpenBoardSwitcher);
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label.contains("vault")).unwrap();
            d.selected = idx;
        }
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);

        if let Some(Popup::Passphrase(p)) = app.popups.last_mut() {
            p.input = "wrong".to_string();
        }
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);
        match app.popups.last() {
            Some(Popup::Passphrase(p)) => assert_eq!(p.error.as_deref(), Some("wrong passphrase")),
            other => panic!("expected passphrase popup to stay open, got {other:?}"),
        }

        if let Some(Popup::Passphrase(p)) = app.popups.last_mut() {
            p.input = "hunter2".to_string();
        }
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);
        assert!(app.popups.is_empty());
        assert_eq!(app.board.name, "vault");
    }

    #[test]
    fn deleting_a_locked_board_removes_its_file_from_disk() {
        let mut app = test_app_with_tasks();
        let dir = tempfile::tempdir().unwrap();
        create_locked_board_for_test(&mut app, &dir, "vault", "hunter2");

        let board = app.db.get_board_by_name("vault").unwrap().unwrap();
        let path = PathBuf::from(board.db_path.clone().unwrap());
        assert!(path.exists());

        app.mode = Mode::Command;
        for c in "board delete vault".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::Confirm(_))));
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

        assert!(!path.exists());
        assert!(app.db.get_board_by_name("vault").unwrap().is_none());
    }

    #[test]
    fn lock_command_drops_connection_and_returns_to_a_plain_board() {
        let mut app = test_app_with_tasks();
        let dir = tempfile::tempdir().unwrap();
        create_locked_board_for_test(&mut app, &dir, "vault", "hunter2");

        app.dispatch(Action::OpenBoardSwitcher);
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label.contains("vault")).unwrap();
            d.selected = idx;
        }
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);
        if let Some(Popup::Passphrase(p)) = app.popups.last_mut() {
            p.input = "hunter2".to_string();
        }
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);
        assert_eq!(app.board.name, "vault");

        app.mode = Mode::Command;
        for c in "lock".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.board.name, "test");
        assert!(!app.unlocked.contains_key(&app.db.get_board_by_name("vault").unwrap().unwrap().id));
    }

    // --- Phase: form-scoped tag picker (Bug 1) ------------------------------

    #[test]
    fn form_new_tag_creation_persists_on_submit_for_a_new_task() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::NewTask);
        {
            let f = match app.popups.last_mut() { Some(Popup::Form(f)) => f, _ => unreachable!() };
            f.title = "tagged task".to_string();
            f.field = Field::Tags;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::FormTagPicker(_))));

        {
            let Some(Popup::FormTagPicker(t)) = app.popups.last_mut() else { unreachable!() };
            t.selected = t.all_tags.len();
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::TextPrompt(_))));
        for c in "urgent".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match app.popups.last() {
            Some(Popup::Form(f)) => assert_eq!(f.tags, vec!["urgent".to_string()]),
            other => panic!("expected the form back on top, got {other:?}"),
        }

        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.popups.is_empty());

        let created = app.columns[0].tasks.iter().find(|t| t.title == "tagged task").expect("task created");
        assert_eq!(created.tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["urgent"]);
    }

    #[test]
    fn form_new_tag_creation_persists_on_submit_for_an_edited_task() {
        let mut app = test_app_with_tasks();
        let id = app.columns[0].tasks[0].id;
        let tag_id = app.store().upsert_tag("home", None).unwrap();
        app.store().set_task_tags(id, &[tag_id]).unwrap();
        let _ = app.reload();

        app.dispatch(Action::EditSelected);
        match app.popups.last() {
            Some(Popup::Form(f)) => assert_eq!(f.tags, vec!["home".to_string()], "edit form must seed current tags"),
            other => panic!("expected an edit-task form, got {other:?}"),
        }

        {
            let f = match app.popups.last_mut() { Some(Popup::Form(f)) => f, _ => unreachable!() };
            f.field = Field::Tags;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        {
            let Some(Popup::FormTagPicker(t)) = app.popups.last_mut() else { unreachable!() };
            t.selected = t.all_tags.len();
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for c in "urgent".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.popups.is_empty());

        let mut names: Vec<String> = app.store().tags_for_task(id).unwrap().into_iter().map(|t| t.name).collect();
        names.sort();
        assert_eq!(names, vec!["home".to_string(), "urgent".to_string()]);
    }

    #[test]
    fn toggling_an_existing_tag_in_the_form_adds_and_removes_it() {
        let mut app = test_app_with_tasks();
        app.store().upsert_tag("home", None).unwrap();
        let _ = app.reload();

        app.dispatch(Action::NewTask);
        {
            let f = match app.popups.last_mut() { Some(Popup::Form(f)) => f, _ => unreachable!() };
            f.title = "t".to_string();
            f.field = Field::Tags;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        {
            let Some(Popup::FormTagPicker(t)) = app.popups.last() else { panic!("expected form tag picker") };
            assert_eq!(t.all_tags, vec!["home".to_string()]);
        }

        // Toggle "home" on: stays open, and the form beneath it picks it up.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::FormTagPicker(_))), "toggle keeps the picker open");
        match app.popups.get(app.popups.len() - 2) {
            Some(Popup::Form(f)) => assert_eq!(f.tags, vec!["home".to_string()]),
            other => panic!("expected the form beneath the picker, got {other:?}"),
        }

        // Toggle it back off.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match app.popups.get(app.popups.len() - 2) {
            Some(Popup::Form(f)) => assert!(f.tags.is_empty()),
            other => panic!("expected the form beneath the picker, got {other:?}"),
        }
    }

    #[test]
    fn the_new_tag_row_label_is_never_stored_as_a_tag() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::NewTask);
        {
            let f = match app.popups.last_mut() { Some(Popup::Form(f)) => f, _ => unreachable!() };
            f.title = "t".to_string();
            f.field = Field::Tags;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        {
            let Some(Popup::FormTagPicker(t)) = app.popups.last() else { panic!("expected form tag picker") };
            assert_eq!(t.selected, 0);
            assert!(t.all_tags.is_empty(), "no board tags exist yet -- only the \"+ new tag…\" row");
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::TextPrompt(_))));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        match app.popups.last() {
            Some(Popup::Form(f)) => assert!(f.tags.is_empty(), "cancelling must not store anything"),
            other => panic!("expected the form back on top, got {other:?}"),
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.popups.is_empty());
        let created = app.columns[0].tasks.iter().find(|t| t.title == "t").expect("task created");
        assert!(created.tags.is_empty());
        assert!(app.store().list_tags().unwrap().iter().all(|t| t.name != "+ new tag…"));
    }

    #[test]
    fn newly_typed_tag_appears_in_all_tags_when_picker_reopens() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::NewTask);
        {
            let f = match app.popups.last_mut() { Some(Popup::Form(f)) => f, _ => unreachable!() };
            f.title = "t".to_string();
            f.field = Field::Tags;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        {
            let Some(Popup::FormTagPicker(t)) = app.popups.last() else { panic!("expected form tag picker") };
            assert_eq!(t.selected, 0);
            assert!(t.all_tags.is_empty());
        }
        // Select "+ new tag…" (at index all_tags.len(), which is 0)
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::TextPrompt(_))));
        for c in "home".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // Back to form
        match app.popups.last() {
            Some(Popup::Form(f)) => assert_eq!(f.tags, vec!["home".to_string()]),
            other => panic!("expected the form back on top, got {other:?}"),
        }
        // Press Enter on Tags field again to reopen the picker
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // Now the picker should have "home" in all_tags and applied
        {
            let Some(Popup::FormTagPicker(t)) = app.popups.last() else { panic!("expected form tag picker") };
            assert!(t.all_tags.contains(&"home".to_string()), "newly typed tag must be in all_tags");
            assert!(t.applied.contains(&"home".to_string()), "newly typed tag must be in applied");
        }
        // Toggle "home" off
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match app.popups.get(app.popups.len() - 2) {
            Some(Popup::Form(f)) => assert!(f.tags.is_empty(), "toggling must remove the tag from the form"),
            other => panic!("expected the form beneath the picker, got {other:?}"),
        }
    }

    // --- Phase: board switcher add/delete, and the bare `:board` menu (Bug 2) --

    #[test]
    fn board_switcher_a_creates_a_new_plain_board() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::OpenBoardSwitcher);
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::TextPrompt(_))));
        for c in "planning".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.popups.is_empty());
        let board = app.db.get_board_by_name("planning").unwrap().expect("board created");
        assert_eq!(board.kind, BoardKind::Plain);
        assert_eq!(app.board.name, "planning", "must switch to the newly created board");
    }

    #[test]
    fn board_switcher_shift_a_starts_a_new_locked_board() {
        let mut app = test_app_with_tasks();
        let dir = tempfile::tempdir().unwrap();
        app.set_data_dir(dir.path().to_path_buf());
        app.dispatch(Action::OpenBoardSwitcher);
        app.handle_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::TextPrompt(_))));
        for c in "vault2".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::Passphrase(_))), "expected the passphrase flow to start");
    }

    #[test]
    fn board_switcher_dd_deletes_the_highlighted_non_current_board() {
        let mut app = test_app_with_tasks();
        app.db.create_board("scratch", BoardKind::Plain).unwrap();
        app.dispatch(Action::OpenBoardSwitcher);
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label == "scratch").unwrap();
            d.selected = idx;
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::Dropdown(_))), "a single 'd' must not delete yet");
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::Confirm(_))));
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.popups.is_empty());
        assert!(app.db.get_board_by_name("scratch").unwrap().is_none());
    }

    #[test]
    fn board_switcher_shift_d_deletes_the_highlighted_board_immediately() {
        let mut app = test_app_with_tasks();
        app.db.create_board("scratch2", BoardKind::Plain).unwrap();
        app.dispatch(Action::OpenBoardSwitcher);
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label == "scratch2").unwrap();
            d.selected = idx;
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::Confirm(_))));
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.popups.is_empty());
        assert!(app.db.get_board_by_name("scratch2").unwrap().is_none());
    }

    #[test]
    fn board_switcher_refuses_to_delete_the_current_board() {
        let mut app = test_app_with_tasks();
        app.db.create_board("scratch3", BoardKind::Plain).unwrap();
        app.dispatch(Action::OpenBoardSwitcher);
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label == "test").unwrap();
            d.selected = idx;
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE));
        assert!(app.popups.is_empty(), "refused, so the switcher closes with a message, same as other actions");
        assert_eq!(app.message.as_deref(), Some("cannot delete the current board; switch away first"));
    }

    #[test]
    fn board_switcher_refuses_to_delete_the_only_board() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::OpenBoardSwitcher);
        app.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE));
        assert!(app.popups.is_empty());
        assert_eq!(app.message.as_deref(), Some("cannot delete the only board"));
    }

    #[test]
    fn bare_board_command_opens_the_menu() {
        let mut app = test_app_with_tasks();
        app.mode = Mode::Command;
        for c in "board".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match app.popups.last() {
            Some(Popup::Dropdown(d)) => {
                let labels: Vec<&str> = d.items.iter().map(|i| i.label.as_str()).collect();
                assert!(labels.contains(&"New board"));
                assert!(labels.contains(&"New locked board"));
                assert!(labels.contains(&"Rename current board"));
                assert!(labels.contains(&"Delete board…"));
                assert!(labels.contains(&"Switch board…"));
                assert!(labels.iter().any(|l| l.starts_with("Current: ")));
                assert!(!labels.contains(&"Lock current board"));
            }
            other => panic!("expected the board menu dropdown, got {other:?}"),
        }
    }

    #[test]
    fn board_menu_new_board_end_to_end() {
        let mut app = test_app_with_tasks();
        app.mode = Mode::Command;
        for c in "board".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label == "New board").unwrap();
            d.selected = idx;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::TextPrompt(_))));
        for c in "fromMenu".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.popups.is_empty());
        assert!(app.db.get_board_by_name("fromMenu").unwrap().is_some());
        assert_eq!(app.board.name, "fromMenu", "must switch to the newly created board");
    }

    #[test]
    fn board_menu_new_locked_board_starts_the_passphrase_flow() {
        let mut app = test_app_with_tasks();
        app.mode = Mode::Command;
        for c in "board".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label == "New locked board").unwrap();
            d.selected = idx;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for c in "vaultFromMenu".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::Passphrase(_))));
    }

    #[test]
    fn board_menu_rename_current_board_end_to_end() {
        let mut app = test_app_with_tasks();
        app.mode = Mode::Command;
        for c in "board".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label == "Rename current board").unwrap();
            d.selected = idx;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match app.popups.last() {
            Some(Popup::TextPrompt(p)) => assert_eq!(p.input, "test", "must be prefilled with the current name"),
            other => panic!("expected a prefilled text prompt, got {other:?}"),
        }
        for _ in 0.."test".len() {
            app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        for c in "renamed-board".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.popups.is_empty());
        assert_eq!(app.board.name, "renamed-board");
        assert!(app.db.get_board_by_name("renamed-board").unwrap().is_some());
    }

    #[test]
    fn board_menu_delete_board_end_to_end() {
        let mut app = test_app_with_tasks();
        app.db.create_board("deleteme", BoardKind::Plain).unwrap();
        app.mode = Mode::Command;
        for c in "board".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label == "Delete board…").unwrap();
            d.selected = idx;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::Dropdown(_))), "expected the board-delete picker");
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label == "deleteme").unwrap();
            d.selected = idx;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.popups.last(), Some(Popup::Confirm(_))));
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.popups.is_empty());
        assert!(app.db.get_board_by_name("deleteme").unwrap().is_none());
    }

    #[test]
    fn board_menu_switch_board_opens_the_switcher() {
        let mut app = test_app_with_tasks();
        app.db.create_board("other", BoardKind::Plain).unwrap();
        app.mode = Mode::Command;
        for c in "board".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label == "Switch board…").unwrap();
            d.selected = idx;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match app.popups.last() {
            Some(Popup::Dropdown(d)) => assert_eq!(d.target, DropdownTarget::Board),
            other => panic!("expected the board switcher, got {other:?}"),
        }
    }

    #[test]
    fn board_menu_offers_lock_only_for_an_unlocked_locked_board() {
        let mut app = test_app_with_tasks();
        let dir = tempfile::tempdir().unwrap();
        create_locked_board_for_test(&mut app, &dir, "vault", "hunter2");
        app.dispatch(Action::OpenBoardSwitcher);
        {
            let Some(Popup::Dropdown(d)) = app.popups.last_mut() else { panic!("expected dropdown") };
            let idx = d.items.iter().position(|i| i.label.contains("vault")).unwrap();
            d.selected = idx;
        }
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);
        if let Some(Popup::Passphrase(p)) = app.popups.last_mut() {
            p.input = "hunter2".to_string();
        }
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);
        assert_eq!(app.board.name, "vault");

        app.mode = Mode::Command;
        for c in "board".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Popup::Dropdown(d)) = app.popups.last() else { panic!("expected dropdown") };
        assert!(d.items.iter().any(|i| i.label == "Lock current board"));
    }

    // --- feature: deadline sort in ToDo/Doing, J/K sort-guard -----------

    fn task_with_deadline(id: TaskId, deadline: Option<i64>, position: i64) -> Task {
        Task {
            id,
            board_id: None,
            title: format!("t{id}"),
            body: String::new(),
            status: Status::ToDo,
            priority: Priority::Normal,
            position,
            created_at: 0,
            updated_at: 0,
            start_date: None,
            time_expected: None,
            deadline,
            deadline_notified_at: None,
            completed_at: None,
            tags: Vec::new(),
        }
    }

    fn task_with_completed(id: TaskId, completed_at: Option<i64>, position: i64) -> Task {
        Task { status: Status::Done, completed_at, ..task_with_deadline(id, None, position) }
    }

    #[test]
    fn deadline_sort_key_orders_overdue_first_and_none_last() {
        let day = 86_400;
        let mut tasks = [
            task_with_deadline(1, None, 0),
            task_with_deadline(2, Some(10 * day), 0),
            task_with_deadline(3, Some(-2 * day), 0),
            task_with_deadline(4, Some(3 * day), 0),
        ];
        tasks.sort_by_key(deadline_sort_key);
        assert_eq!(tasks.iter().map(|t| t.id).collect::<Vec<_>>(), vec![3, 4, 2, 1]);
    }

    #[test]
    fn deadline_sort_key_tiebreaks_equal_deadlines_by_position() {
        let day = 86_400;
        let mut tasks = [
            task_with_deadline(1, Some(5 * day), 1),
            task_with_deadline(2, Some(5 * day), 0),
        ];
        tasks.sort_by_key(deadline_sort_key);
        assert_eq!(tasks.iter().map(|t| t.id).collect::<Vec<_>>(), vec![2, 1]);
    }

    #[test]
    fn completed_sort_key_orders_most_recent_first() {
        let mut tasks = [
            task_with_completed(1, Some(100), 0),
            task_with_completed(2, Some(300), 0),
            task_with_completed(3, Some(200), 0),
        ];
        tasks.sort_by_key(completed_sort_key);
        assert_eq!(tasks.iter().map(|t| t.id).collect::<Vec<_>>(), vec![2, 3, 1]);
    }

    #[test]
    fn is_archived_boundary_is_exactly_five_days() {
        let now = 10 * ARCHIVE_AFTER_SECS;
        let exactly_five_days = task_with_completed(1, Some(now - ARCHIVE_AFTER_SECS), 0);
        assert!(!is_archived(&exactly_five_days, now), "exactly 5 days old must not be archived yet");

        let one_second_past = task_with_completed(2, Some(now - ARCHIVE_AFTER_SECS - 1), 0);
        assert!(is_archived(&one_second_past, now));

        let never_completed = task_with_completed(3, None, 0);
        assert!(!is_archived(&never_completed, now));
    }

    #[test]
    fn same_sort_bucket_compares_deadline_for_todo_doing_and_completed_at_for_done() {
        let day = 86_400;
        let a = task_with_deadline(1, Some(3 * day), 0);
        let b = task_with_deadline(2, Some(3 * day), 1);
        let c = task_with_deadline(3, Some(4 * day), 2);
        assert!(same_sort_bucket(0, &a, &b));
        assert!(!same_sort_bucket(0, &a, &c));

        let d = task_with_completed(4, Some(500), 0);
        let e = task_with_completed(5, Some(500), 1);
        let f = task_with_completed(6, Some(600), 2);
        assert!(same_sort_bucket(2, &d, &e));
        assert!(!same_sort_bucket(2, &d, &f));
    }

    fn new_task_with_deadline(title: &str, deadline: Option<i64>) -> DomainNewTask {
        DomainNewTask {
            title: title.to_string(),
            body: String::new(),
            status: Status::ToDo,
            priority: DomainPriority::Normal,
            start_date: None,
            time_expected: None,
            deadline,
        }
    }

    #[test]
    fn todo_column_loads_sorted_by_deadline_ascending_none_last() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("test", BoardKind::Plain).unwrap();
        let day = 86_400;
        {
            let store = db.store_for(board_id);
            // Scrambled creation order on purpose.
            store.create_task(new_task_with_deadline("none", None)).unwrap();
            store.create_task(new_task_with_deadline("distant", Some(10 * day))).unwrap();
            store.create_task(new_task_with_deadline("overdue", Some(-2 * day))).unwrap();
            store.create_task(new_task_with_deadline("near", Some(3 * day))).unwrap();
        }
        let board = db.get_board_by_name("test").unwrap().unwrap();
        let app = App::new(Config::default(), db, board).unwrap();

        let titles: Vec<&str> = app.columns[0].tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["overdue", "near", "distant", "none"]);
    }

    #[test]
    fn jk_blocked_between_different_deadlines() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("test", BoardKind::Plain).unwrap();
        let day = 86_400;
        {
            let store = db.store_for(board_id);
            store.create_task(new_task_with_deadline("soon", Some(day))).unwrap();
            store.create_task(new_task_with_deadline("later", Some(5 * day))).unwrap();
        }
        let board = db.get_board_by_name("test").unwrap().unwrap();
        let mut app = App::new(Config::default(), db, board).unwrap();

        let before: Vec<String> = app.columns[0].tasks.iter().map(|t| t.title.clone()).collect();
        app.columns[0].selected = 0; // "soon", the earlier deadline
        app.dispatch(Action::ReorderTaskDown);

        let after: Vec<String> = app.columns[0].tasks.iter().map(|t| t.title.clone()).collect();
        assert_eq!(before, after, "different-deadline rows must not swap");
        assert!(app.message.as_deref().unwrap_or("").contains("sorted by deadline"), "{:?}", app.message);
    }

    #[test]
    fn jk_allowed_between_equal_deadlines() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("test", BoardKind::Plain).unwrap();
        let day = 86_400;
        {
            let store = db.store_for(board_id);
            store.create_task(new_task_with_deadline("a", Some(2 * day))).unwrap();
            store.create_task(new_task_with_deadline("b", Some(2 * day))).unwrap();
        }
        let board = db.get_board_by_name("test").unwrap().unwrap();
        let mut app = App::new(Config::default(), db, board).unwrap();
        assert_eq!(app.columns[0].tasks[0].title, "a");

        app.columns[0].selected = 0;
        app.dispatch(Action::ReorderTaskDown);

        assert_eq!(app.columns[0].tasks[0].title, "b", "same-deadline rows may swap");
        assert_eq!(app.columns[0].tasks[1].title, "a");
    }

    #[test]
    fn jk_allowed_between_two_tasks_with_no_deadline() {
        let mut app = test_app_with_tasks(); // "a", "b", "c", all with no deadline
        app.columns[0].selected = 0;
        app.dispatch(Action::ReorderTaskDown);
        assert_eq!(app.columns[0].tasks[0].title, "b");
        assert_eq!(app.columns[0].tasks[1].title, "a");
    }

    // --- feature: completed_at set/cleared on every route into/out of Done --

    fn task_completed_at(app: &App, id: TaskId) -> Option<i64> {
        app.db.store_for(app.board.id).get_task(id).unwrap().completed_at
    }

    /// Focuses whichever column `id` is currently in and selects it there,
    /// so a follow-up `MoveTaskNextColumn`/`MoveTaskPrevColumn` acts on the
    /// same task rather than on whatever else now sits at the old
    /// selection index.
    fn focus_task(app: &mut App, id: TaskId) {
        for (idx, col) in app.columns.iter().enumerate() {
            if let Some(pos) = col.tasks.iter().position(|t| t.id == id) {
                app.focused = idx;
                app.columns[idx].selected = pos;
                return;
            }
        }
        panic!("task {id} not found in any column");
    }

    #[test]
    fn h_l_route_sets_and_clears_completed_at() {
        let mut app = test_app_with_tasks();
        let id = app.columns[0].tasks[0].id;
        focus_task(&mut app, id);

        app.dispatch(Action::MoveTaskNextColumn); // ToDo -> Doing
        assert_eq!(task_completed_at(&app, id), None);
        focus_task(&mut app, id);
        app.dispatch(Action::MoveTaskNextColumn); // Doing -> Done
        assert!(task_completed_at(&app, id).is_some());

        focus_task(&mut app, id);
        app.dispatch(Action::MoveTaskPrevColumn); // Done -> Doing
        assert_eq!(task_completed_at(&app, id), None);
    }

    #[test]
    fn dropdown_route_sets_completed_at() {
        let mut app = test_app_with_tasks();
        let id = app.columns[0].tasks[0].id;
        app.dispatch(Action::OpenColumnDropdown);
        if let Some(Popup::Dropdown(d)) = app.popups.last_mut() {
            let idx = d.items.iter().position(|i| i.label == "Done").unwrap();
            d.selected = idx;
        }
        let outcome = app.popups.last_mut().unwrap().handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.apply_popup_outcome(outcome);
        assert!(task_completed_at(&app, id).is_some());
    }

    #[test]
    fn edit_form_status_change_route_sets_completed_at() {
        let mut app = test_app_with_tasks();
        let id = app.columns[0].tasks[0].id;
        app.dispatch(Action::EditSelected);
        if let Some(Popup::Form(f)) = app.popups.last_mut() {
            f.status = Status::Done;
        }
        let outcome = app
            .popups
            .last_mut()
            .unwrap()
            .handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        app.apply_popup_outcome(outcome);
        assert!(task_completed_at(&app, id).is_some());
    }

    #[test]
    fn undo_redo_of_a_move_to_done_toggles_completed_at() {
        let mut app = test_app_with_tasks();
        let id = app.columns[0].tasks[0].id;
        focus_task(&mut app, id);
        app.dispatch(Action::MoveTaskNextColumn);
        focus_task(&mut app, id);
        app.dispatch(Action::MoveTaskNextColumn);
        assert!(task_completed_at(&app, id).is_some());

        app.dispatch(Action::Undo);
        assert_eq!(task_completed_at(&app, id), None);

        app.dispatch(Action::Redo);
        assert!(task_completed_at(&app, id).is_some());
    }

    // --- feature: auto-archive & the archive browser ----------------------

    #[test]
    fn freshly_completed_done_task_stays_on_the_board() {
        let mut app = test_app_with_tasks();
        let id = app.columns[0].tasks[0].id;
        focus_task(&mut app, id);
        app.dispatch(Action::MoveTaskNextColumn);
        focus_task(&mut app, id);
        app.dispatch(Action::MoveTaskNextColumn);
        assert_eq!(app.columns[2].tasks.len(), 1);
    }

    #[test]
    fn shift_a_opens_the_archive_browser_popup() {
        let mut app = test_app_with_tasks();
        app.dispatch(Action::OpenArchiveBrowser);
        assert!(matches!(app.popups.last(), Some(Popup::Archive(_))));
    }

    #[test]
    fn archive_browser_refused_from_the_notes_pane() {
        let mut app = test_app_with_tasks();
        app.pane = Pane::Notes;
        app.dispatch(Action::OpenArchiveBrowser);
        assert!(app.popups.is_empty());
        assert_eq!(app.message.as_deref(), Some("archive is only for the board"));
    }
}
