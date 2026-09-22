use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use crate::domain::board::{Board, BoardId, BoardKind};
use crate::domain::note::{Note, NoteId};
use crate::domain::task::{NewTask, Status, Tag, TagId, Task, TaskId, TaskPatch};
use crate::storage::task_store_impl as shared;
use crate::storage::{migrations, StorageError, TaskStore};

type BoardRow = (
    BoardId,
    String,
    String,
    Option<String>,
    Option<Vec<u8>>,
    Option<u32>,
    Option<u32>,
    Option<u32>,
    i64,
    i64,
    i64,
);

fn board_from_row(raw: BoardRow) -> Result<Board, StorageError> {
    let (id, name, kind, db_path, kdf_salt, kdf_m_cost, kdf_t_cost, kdf_p_cost, position, created_at, updated_at) = raw;
    let kind = BoardKind::from_str(&kind).ok_or_else(|| StorageError::InvalidEnum(kind.clone()))?;
    Ok(Board {
        id,
        name,
        kind,
        db_path,
        kdf_salt,
        kdf_m_cost,
        kdf_t_cost,
        kdf_p_cost,
        position,
        created_at,
        updated_at,
    })
}

pub struct MainDb {
    conn: Connection,
}

impl MainDb {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
                crate::storage::harden_dir(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        // Before the first write, so the WAL/SHM sidecars inherit 0600.
        crate::storage::harden_file(path)?;
        Self::init_conn(&conn, true)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()?;
        Self::init_conn(&conn, false)?;
        Ok(Self { conn })
    }

    fn init_conn(conn: &Connection, wal: bool) -> Result<(), StorageError> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        if wal {
            conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        }
        conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
        migrations::apply(conn, migrations::MIGRATIONS_MAIN)?;
        Ok(())
    }

    pub fn list_boards(&self) -> Result<Vec<Board>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, db_path, kdf_salt, kdf_m_cost, kdf_t_cost, kdf_p_cost, position, created_at, updated_at
             FROM boards ORDER BY position, id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
            ))
        })?;
        let mut boards = Vec::new();
        for row in rows {
            boards.push(board_from_row(row?)?);
        }
        Ok(boards)
    }

    pub fn get_board_by_name(&self, name: &str) -> Result<Option<Board>, StorageError> {
        let raw: Option<BoardRow> = self
            .conn
            .query_row(
                "SELECT id, name, kind, db_path, kdf_salt, kdf_m_cost, kdf_t_cost, kdf_p_cost, position, created_at, updated_at
                 FROM boards WHERE name = ?1",
                rusqlite::params![name],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ))
                },
            )
            .optional()?;
        raw.map(board_from_row).transpose()
    }

    pub fn create_board(&self, name: &str, kind: BoardKind) -> Result<BoardId, StorageError> {
        let now = chrono::Utc::now().timestamp();
        let next_position: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM boards",
            [],
            |row| row.get(0),
        )?;
        let result = self.conn.execute(
            "INSERT INTO boards (name, kind, position, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            rusqlite::params![name, kind.as_str(), next_position, now],
        );
        match result {
            Ok(_) => Ok(self.conn.last_insert_rowid()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(StorageError::BoardExists(name.to_string()))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn rename_board(&self, id: BoardId, new_name: &str) -> Result<(), StorageError> {
        let now = chrono::Utc::now().timestamp();
        let result = self.conn.execute(
            "UPDATE boards SET name = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![new_name, now, id],
        );
        match result {
            Ok(0) => Err(StorageError::NotFound),
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(StorageError::BoardExists(new_name.to_string()))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn delete_board(&self, id: BoardId) -> Result<(), StorageError> {
        let changed = self
            .conn
            .execute("DELETE FROM boards WHERE id = ?1", rusqlite::params![id])?;
        if changed == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    pub fn set_board_crypto(
        &self,
        id: BoardId,
        db_path: &str,
        salt: &[u8],
        m: u32,
        t: u32,
        p: u32,
    ) -> Result<(), StorageError> {
        let now = chrono::Utc::now().timestamp();
        let changed = self.conn.execute(
            "UPDATE boards SET db_path = ?1, kdf_salt = ?2, kdf_m_cost = ?3, kdf_t_cost = ?4, kdf_p_cost = ?5, updated_at = ?6
             WHERE id = ?7",
            rusqlite::params![db_path, salt, m, t, p, now, id],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    pub fn store_for(&self, board_id: BoardId) -> PlainBoardStore<'_> {
        PlainBoardStore {
            conn: &self.conn,
            board_id,
        }
    }
}

pub struct PlainBoardStore<'a> {
    conn: &'a Connection,
    board_id: BoardId,
}

impl<'a> TaskStore for PlainBoardStore<'a> {
    fn list_tasks(&self, status: Status) -> Result<Vec<Task>, StorageError> {
        shared::list_tasks(self.conn, Some(self.board_id), status)
    }

    fn get_task(&self, id: TaskId) -> Result<Task, StorageError> {
        shared::get_task(self.conn, Some(self.board_id), id)
    }

    fn create_task(&self, draft: NewTask) -> Result<TaskId, StorageError> {
        shared::create_task(self.conn, Some(self.board_id), draft)
    }

    fn update_task(&self, id: TaskId, patch: TaskPatch) -> Result<(), StorageError> {
        shared::update_task(self.conn, Some(self.board_id), id, patch)
    }

    fn move_task(&self, id: TaskId, to: Status, index: i64) -> Result<(), StorageError> {
        shared::move_task(self.conn, Some(self.board_id), id, to, index)
    }

    fn reorder(&self, status: Status, ordered: &[TaskId]) -> Result<(), StorageError> {
        shared::reorder(self.conn, Some(self.board_id), status, ordered)
    }

    fn delete_task(&self, id: TaskId) -> Result<(), StorageError> {
        shared::delete_task(self.conn, Some(self.board_id), id)
    }

    fn list_notes(&self) -> Result<Vec<Note>, StorageError> {
        shared::list_notes(self.conn, Some(self.board_id))
    }

    fn create_note(&self, body: &str) -> Result<NoteId, StorageError> {
        shared::create_note(self.conn, Some(self.board_id), body)
    }

    fn update_note(&self, id: NoteId, body: &str) -> Result<(), StorageError> {
        shared::update_note(self.conn, Some(self.board_id), id, body)
    }

    fn delete_note(&self, id: NoteId) -> Result<(), StorageError> {
        shared::delete_note(self.conn, Some(self.board_id), id)
    }

    fn promote_note(&self, id: NoteId, status: Status) -> Result<TaskId, StorageError> {
        shared::promote_note(self.conn, Some(self.board_id), id, status)
    }

    fn list_tags(&self) -> Result<Vec<Tag>, StorageError> {
        shared::list_tags(self.conn, Some(self.board_id))
    }

    fn upsert_tag(&self, name: &str, color: Option<&str>) -> Result<TagId, StorageError> {
        shared::upsert_tag(self.conn, Some(self.board_id), name, color)
    }

    fn rename_tag(&self, id: TagId, new_name: &str) -> Result<(), StorageError> {
        shared::rename_tag(self.conn, Some(self.board_id), id, new_name)
    }

    fn delete_tag(&self, id: TagId) -> Result<(), StorageError> {
        shared::delete_tag(self.conn, Some(self.board_id), id)
    }

    fn tags_for_task(&self, id: TaskId) -> Result<Vec<Tag>, StorageError> {
        shared::tags_for_task(self.conn, id)
    }

    fn set_task_tags(&self, id: TaskId, tags: &[TagId]) -> Result<(), StorageError> {
        shared::set_task_tags(self.conn, id, tags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::task::Priority;
    use crate::storage::task_store_impl::shared_tests;

    #[test]
    fn migrations_are_idempotent_and_versioned() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(migrations::current_version(&conn).unwrap(), 0);
        migrations::apply(&conn, migrations::MIGRATIONS_MAIN).unwrap();
        assert_eq!(migrations::current_version(&conn).unwrap(), 2);
        migrations::apply(&conn, migrations::MIGRATIONS_MAIN).unwrap();
        assert_eq!(migrations::current_version(&conn).unwrap(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn main_db_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("main.db");
        let _db = MainDb::open(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "main.db holds plaintext tasks; it must be owner-only");
        let dmode = std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode();
        assert_eq!(dmode & 0o777, 0o700);
    }

    #[test]
    fn board_create_list_rename_delete() {
        let db = MainDb::open_in_memory().unwrap();
        let id = db.create_board("alpha", BoardKind::Plain).unwrap();

        let boards = db.list_boards().unwrap();
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].id, id);
        assert_eq!(boards[0].name, "alpha");
        assert_eq!(boards[0].kind, BoardKind::Plain);

        let found = db.get_board_by_name("alpha").unwrap().unwrap();
        assert_eq!(found.id, id);
        assert!(db.get_board_by_name("nope").unwrap().is_none());

        db.rename_board(id, "beta").unwrap();
        let renamed = db.get_board_by_name("beta").unwrap().unwrap();
        assert_eq!(renamed.id, id);
        assert!(db.get_board_by_name("alpha").unwrap().is_none());

        db.delete_board(id).unwrap();
        assert!(db.list_boards().unwrap().is_empty());
    }

    #[test]
    fn duplicate_board_name_rejected() {
        let db = MainDb::open_in_memory().unwrap();
        db.create_board("alpha", BoardKind::Plain).unwrap();
        let err = db.create_board("alpha", BoardKind::Plain).unwrap_err();
        assert!(matches!(err, StorageError::BoardExists(name) if name == "alpha"));
    }

    #[test]
    fn create_board_and_task_round_trip() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        let store = db.store_for(board_id);
        let task_id = store
            .create_task(NewTask {
                title: "write report".into(),
                body: "".into(),
                status: Status::ToDo,
                priority: Priority::Normal,
                start_date: None,
                deadline: None,
            })
            .unwrap();
        let task = store.get_task(task_id).unwrap();
        assert_eq!(task.title, "write report");
        assert_eq!(task.board_id, Some(board_id));
        assert_eq!(task.status, Status::ToDo);
        assert_eq!(task.priority, Priority::Normal);
        assert_eq!(task.position, 0);
    }

    #[test]
    fn positions_are_sequential_on_append() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        let store = db.store_for(board_id);
        let mut ids = Vec::new();
        for title in ["a", "b", "c"] {
            ids.push(
                store
                    .create_task(NewTask {
                        title: title.into(),
                        body: "".into(),
                        status: Status::ToDo,
                        priority: Priority::Normal,
                        start_date: None,
                        deadline: None,
                    })
                    .unwrap(),
            );
        }
        let tasks = store.list_tasks(Status::ToDo).unwrap();
        let positions: Vec<i64> = tasks.iter().map(|t| t.position).collect();
        assert_eq!(positions, vec![0, 1, 2]);
    }

    fn new_task(title: &str, status: Status) -> NewTask {
        NewTask {
            title: title.into(),
            body: "".into(),
            status,
            priority: Priority::Normal,
            start_date: None,
            deadline: None,
        }
    }

    #[test]
    fn move_task_across_columns_keeps_both_dense() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        let store = db.store_for(board_id);
        let t0 = store.create_task(new_task("t0", Status::ToDo)).unwrap();
        let t1 = store.create_task(new_task("t1", Status::ToDo)).unwrap();
        let d0 = store.create_task(new_task("d0", Status::Doing)).unwrap();

        store.move_task(t0, Status::Doing, 1).unwrap();

        let todo = store.list_tasks(Status::ToDo).unwrap();
        assert_eq!(todo.len(), 1);
        assert_eq!(todo[0].id, t1);
        assert_eq!(todo[0].position, 0);

        let doing = store.list_tasks(Status::Doing).unwrap();
        assert_eq!(doing.len(), 2);
        assert_eq!(doing[0].id, d0);
        assert_eq!(doing[0].position, 0);
        assert_eq!(doing[1].id, t0);
        assert_eq!(doing[1].position, 1);
    }

    #[test]
    fn move_task_clamps_out_of_range_index() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        let store = db.store_for(board_id);
        let a = store.create_task(new_task("a", Status::ToDo)).unwrap();
        let b = store.create_task(new_task("b", Status::Doing)).unwrap();

        store.move_task(a, Status::Doing, 999).unwrap();
        let doing = store.list_tasks(Status::Doing).unwrap();
        assert_eq!(doing.len(), 2);
        assert_eq!(doing[0].id, b);
        assert_eq!(doing[1].id, a);
        assert_eq!(doing[1].position, 1);

        store.move_task(a, Status::Doing, -5).unwrap();
        let doing = store.list_tasks(Status::Doing).unwrap();
        assert_eq!(doing[0].id, a);
        assert_eq!(doing[0].position, 0);
        assert_eq!(doing[1].id, b);
        assert_eq!(doing[1].position, 1);
    }

    #[test]
    fn move_task_within_same_column() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        let store = db.store_for(board_id);
        let a = store.create_task(new_task("a", Status::ToDo)).unwrap();
        let b = store.create_task(new_task("b", Status::ToDo)).unwrap();
        let c = store.create_task(new_task("c", Status::ToDo)).unwrap();

        store.move_task(b, Status::ToDo, 0).unwrap();
        let todo = store.list_tasks(Status::ToDo).unwrap();
        assert_eq!(
            todo.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![b, a, c]
        );
        assert_eq!(
            todo.iter().map(|t| t.position).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );

        store.move_task(b, Status::ToDo, 2).unwrap();
        let todo = store.list_tasks(Status::ToDo).unwrap();
        assert_eq!(
            todo.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![a, c, b]
        );
        assert_eq!(
            todo.iter().map(|t| t.position).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn reorder_rewrites_positions() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        let store = db.store_for(board_id);
        let a = store.create_task(new_task("a", Status::ToDo)).unwrap();
        let b = store.create_task(new_task("b", Status::ToDo)).unwrap();
        let c = store.create_task(new_task("c", Status::ToDo)).unwrap();

        store.reorder(Status::ToDo, &[c, a, b]).unwrap();
        let todo = store.list_tasks(Status::ToDo).unwrap();
        assert_eq!(
            todo.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![c, a, b]
        );
        assert_eq!(
            todo.iter().map(|t| t.position).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn update_task_partial_patch() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        let store = db.store_for(board_id);
        let task_id = store
            .create_task(new_task("original", Status::ToDo))
            .unwrap();
        let before = store.get_task(task_id).unwrap();

        // Force the stored updated_at further into the past so the bump the
        // patch produces is provable regardless of clock resolution.
        db.conn
            .execute(
                "UPDATE tasks SET updated_at = updated_at - 10 WHERE id = ?1",
                rusqlite::params![task_id],
            )
            .unwrap();
        let baseline_updated_at = before.updated_at - 10;

        store
            .update_task(
                task_id,
                TaskPatch {
                    title: Some("renamed".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let after = store.get_task(task_id).unwrap();
        assert_eq!(after.title, "renamed");
        assert_eq!(after.body, before.body);
        assert_eq!(after.status, before.status);
        assert_eq!(after.priority, before.priority);
        assert!(after.updated_at > baseline_updated_at);
    }

    #[test]
    fn delete_task_closes_gap() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        let store = db.store_for(board_id);
        let a = store.create_task(new_task("a", Status::ToDo)).unwrap();
        let b = store.create_task(new_task("b", Status::ToDo)).unwrap();
        let c = store.create_task(new_task("c", Status::ToDo)).unwrap();

        store.delete_task(b).unwrap();
        let todo = store.list_tasks(Status::ToDo).unwrap();
        assert_eq!(todo.iter().map(|t| t.id).collect::<Vec<_>>(), vec![a, c]);
        assert_eq!(
            todo.iter().map(|t| t.position).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn delete_board_cascades_tasks_and_notes() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        let store = db.store_for(board_id);
        let task_id = store.create_task(new_task("a", Status::ToDo)).unwrap();
        let note_id = store.create_note("a note").unwrap();

        db.delete_board(board_id).unwrap();

        let task_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE id = ?1",
                rusqlite::params![task_id],
                |row| row.get(0),
            )
            .unwrap();
        let note_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM notes WHERE id = ?1",
                rusqlite::params![note_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(task_count, 0);
        assert_eq!(note_count, 0);
    }

    #[test]
    fn note_crud() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        let store = db.store_for(board_id);

        let note_id = store.create_note("first note").unwrap();
        let notes = store.list_notes().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, note_id);
        assert_eq!(notes[0].body, "first note");
        assert_eq!(notes[0].promoted_task_id, None);

        store.update_note(note_id, "edited note").unwrap();
        let notes = store.list_notes().unwrap();
        assert_eq!(notes[0].body, "edited note");

        store.delete_note(note_id).unwrap();
        assert!(store.list_notes().unwrap().is_empty());
    }

    #[test]
    fn promote_note_splits_title_and_body() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        let store = db.store_for(board_id);

        let note_id = store
            .create_note("Buy groceries\nmilk, eggs, bread")
            .unwrap();

        let task_id = store.promote_note(note_id, Status::ToDo).unwrap();
        let task = store.get_task(task_id).unwrap();
        assert_eq!(task.title, "Buy groceries");
        assert_eq!(task.body, "milk, eggs, bread");
        assert_eq!(task.status, Status::ToDo);

        let notes = store.list_notes().unwrap();
        assert_eq!(notes[0].promoted_task_id, Some(task_id));
    }

    #[test]
    fn invalid_status_string_surfaces_invalid_enum_not_panic() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();

        db.conn
            .execute_batch("PRAGMA ignore_check_constraints = ON;")
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO tasks (board_id, title, body, status, priority, position, created_at, updated_at)
                 VALUES (?1, 'x', '', 'bogus', 'normal', 0, 0, 0)",
                rusqlite::params![board_id],
            )
            .unwrap();
        let task_id = db.conn.last_insert_rowid();
        db.conn
            .execute_batch("PRAGMA ignore_check_constraints = OFF;")
            .unwrap();

        let store = db.store_for(board_id);
        let result = store.get_task(task_id);
        assert!(matches!(result, Err(StorageError::InvalidEnum(_))));
    }

    #[test]
    fn task_store_is_object_safe() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        let store = db.store_for(board_id);
        let dyn_store: &dyn TaskStore = &store;
        assert!(dyn_store.list_tasks(Status::ToDo).unwrap().is_empty());
    }

    #[test]
    fn create_read_task_with_and_without_dates() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        shared_tests::create_read_task_with_and_without_dates(&db.store_for(board_id));
    }

    #[test]
    fn clear_deadline_via_patch_leaves_title_untouched() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        shared_tests::clear_deadline_via_patch_leaves_title_untouched(&db.store_for(board_id));
    }

    #[test]
    fn tag_upsert_is_idempotent_and_rejects_empty_name() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        shared_tests::tag_upsert_is_idempotent_and_rejects_empty_name(&db.store_for(board_id));
    }

    #[test]
    fn tag_rename_and_delete() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        shared_tests::tag_rename_and_delete(&db.store_for(board_id));
    }

    #[test]
    fn set_task_tags_replaces_whole_set_and_reads_back() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        shared_tests::set_task_tags_replaces_whole_set_and_reads_back(&db.store_for(board_id));
    }

    #[test]
    fn deleting_task_cascades_its_tags() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        shared_tests::deleting_task_cascades_its_tags(&db.store_for(board_id));
    }

    #[test]
    fn tags_load_with_list_tasks_for_many_tasks() {
        let db = MainDb::open_in_memory().unwrap();
        let board_id = db.create_board("alpha", BoardKind::Plain).unwrap();
        shared_tests::tags_load_with_list_tasks_for_many_tasks(&db.store_for(board_id));
    }

    #[test]
    fn two_boards_do_not_share_tags() {
        let db = MainDb::open_in_memory().unwrap();
        let board_a = db.create_board("alpha", BoardKind::Plain).unwrap();
        let board_b = db.create_board("beta", BoardKind::Plain).unwrap();
        let store_a = db.store_for(board_a);
        let store_b = db.store_for(board_b);

        store_a.upsert_tag("shared-name", None).unwrap();
        store_b.upsert_tag("shared-name", None).unwrap();

        assert_eq!(store_a.list_tags().unwrap().len(), 1);
        assert_eq!(store_b.list_tags().unwrap().len(), 1);
    }
}
