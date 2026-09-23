//! A single locked board's own SQLCipher-encrypted database file.
//!
//! Connection open order (verified against this build's bundled SQLCipher
//! by the tests below actually round-tripping through it):
//! 1. `Connection::open`.
//! 2. `PRAGMA key = "x'<64 lowercase hex>'"` -- MUST be the first
//!    statement executed on the connection; SQLCipher only honours a key
//!    set before anything else touches the file.
//! 3. `PRAGMA cipher_log_level = NONE` -- silences SQLCipher's stderr
//!    diagnostics on a failed decrypt (which otherwise corrupt the TUI's
//!    screen). It cannot go before `key` (rule 2 overrides), and going
//!    after it works fine: it configures how the now-attached codec logs,
//!    and takes effect before the probe read below ever runs.
//! 4. A probe read (`SELECT count(*) FROM sqlite_master`) -- a wrong key
//!    does NOT fail at `PRAGMA key`; it only fails on the first real read,
//!    so this is what turns a wrong passphrase into a clean error instead
//!    of a later, confusing SQLite failure.
//! 5. The rest of the standard pragmas (`foreign_keys`, `journal_mode`,
//!    `synchronous`) and the schema migration.

use std::path::Path;

use rusqlite::Connection;

use crate::config::JournalMode;
use crate::domain::note::{Note, NoteId};
use crate::domain::task::{NewTask, Status, Tag, TagId, Task, TaskId, TaskPatch};
use crate::storage::task_store_impl as shared;
use crate::storage::{migrations, StorageError, TaskStore};

#[derive(Debug)]
pub struct LockedDb {
    conn: Connection,
}

impl LockedDb {
    /// Creates a new encrypted board file at `path`. Fails (via an I/O
    /// `AlreadyExists` error) if the path already exists, so an existing
    /// file can never be silently overwritten.
    pub fn create(
        path: &Path,
        key_hex: &str,
        board_name: &str,
        journal_mode: JournalMode,
    ) -> Result<Self, StorageError> {
        if path.exists() {
            return Err(StorageError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} already exists", path.display()),
            )));
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
                crate::storage::harden_dir(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        // Before the first write, so the WAL/SHM sidecars inherit 0600.
        crate::storage::harden_file(path)?;
        Self::key_and_open(&conn, key_hex, journal_mode)?;
        migrations::apply(&conn, migrations::MIGRATIONS_BOARD)?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO board_meta (id, name, created_at) VALUES (1, ?1, ?2)",
            rusqlite::params![board_name, now],
        )?;
        Ok(Self { conn })
    }

    /// Opens an existing encrypted board file. A wrong `key_hex` surfaces
    /// as `StorageError::WrongPassphrase`, never a panic or a bare SQLite
    /// error -- see the probe read in `key_and_open`.
    pub fn open(path: &Path, key_hex: &str, journal_mode: JournalMode) -> Result<Self, StorageError> {
        let conn = Connection::open(path)?;
        crate::storage::harden_file(path)?;
        Self::key_and_open(&conn, key_hex, journal_mode)?;
        migrations::apply(&conn, migrations::MIGRATIONS_BOARD)?;
        Ok(Self { conn })
    }

    /// Keys the connection and probes it. See the module doc comment for
    /// why this exact pragma order matters. `journal_mode` picks `WAL`
    /// (default) or `DELETE` -- see `Config::journal_mode` and
    /// `MainDb::init_conn` for why `DELETE` matters under Dropbox/Syncthing;
    /// setting it here converts an existing WAL board file and checkpoints
    /// (then removes) any leftover `-wal`/`-shm` as part of the same pragma.
    fn key_and_open(conn: &Connection, key_hex: &str, journal_mode: JournalMode) -> Result<(), StorageError> {
        conn.execute_batch(&format!("PRAGMA key = \"x'{key_hex}'\";"))?;
        conn.execute_batch("PRAGMA cipher_log_level = NONE;")?;

        let probe: Result<i64, rusqlite::Error> =
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0));
        probe.map_err(|_| StorageError::WrongPassphrase)?;

        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        match journal_mode {
            JournalMode::Wal => conn.execute_batch("PRAGMA journal_mode = WAL;")?,
            JournalMode::Delete => conn.execute_batch("PRAGMA journal_mode = DELETE;")?,
        }
        conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
        Ok(())
    }

    pub fn store(&self) -> LockedBoardStore<'_> {
        LockedBoardStore { conn: &self.conn }
    }
}

pub struct LockedBoardStore<'a> {
    conn: &'a Connection,
}

impl<'a> TaskStore for LockedBoardStore<'a> {
    fn list_tasks(&self, status: Status) -> Result<Vec<Task>, StorageError> {
        shared::list_tasks(self.conn, None, status)
    }
    fn get_task(&self, id: TaskId) -> Result<Task, StorageError> {
        shared::get_task(self.conn, None, id)
    }
    fn create_task(&self, draft: NewTask) -> Result<TaskId, StorageError> {
        shared::create_task(self.conn, None, draft)
    }
    fn update_task(&self, id: TaskId, patch: TaskPatch) -> Result<(), StorageError> {
        shared::update_task(self.conn, None, id, patch)
    }
    fn move_task(&self, id: TaskId, to: Status, index: i64) -> Result<(), StorageError> {
        shared::move_task(self.conn, None, id, to, index)
    }
    fn reorder(&self, status: Status, ordered: &[TaskId]) -> Result<(), StorageError> {
        shared::reorder(self.conn, None, status, ordered)
    }
    fn delete_task(&self, id: TaskId) -> Result<(), StorageError> {
        shared::delete_task(self.conn, None, id)
    }
    fn list_notes(&self) -> Result<Vec<Note>, StorageError> {
        shared::list_notes(self.conn, None)
    }
    fn create_note(&self, body: &str) -> Result<NoteId, StorageError> {
        shared::create_note(self.conn, None, body)
    }
    fn update_note(&self, id: NoteId, body: &str) -> Result<(), StorageError> {
        shared::update_note(self.conn, None, id, body)
    }
    fn delete_note(&self, id: NoteId) -> Result<(), StorageError> {
        shared::delete_note(self.conn, None, id)
    }
    fn promote_note(&self, id: NoteId, status: Status) -> Result<TaskId, StorageError> {
        shared::promote_note(self.conn, None, id, status)
    }
    fn list_tags(&self) -> Result<Vec<Tag>, StorageError> {
        shared::list_tags(self.conn, None)
    }
    fn upsert_tag(&self, name: &str, color: Option<&str>) -> Result<TagId, StorageError> {
        shared::upsert_tag(self.conn, None, name, color)
    }
    fn rename_tag(&self, id: TagId, new_name: &str) -> Result<(), StorageError> {
        shared::rename_tag(self.conn, None, id, new_name)
    }
    fn delete_tag(&self, id: TagId) -> Result<(), StorageError> {
        shared::delete_tag(self.conn, None, id)
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
    use crate::crypto::{self, Kdf};
    use crate::domain::task::Priority;
    use crate::storage::task_store_impl::shared_tests;

    fn tiny_kdf() -> Kdf {
        Kdf { m_cost: 8, t_cost: 1, p_cost: 1 }
    }

    fn key_hex_for(passphrase: &str, salt: &[u8]) -> String {
        let key = crypto::derive_key(passphrase, salt, tiny_kdf()).unwrap();
        crypto::key_to_hex(&key).to_string()
    }

    #[test]
    fn create_then_open_round_trips_a_task() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);

        {
            let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
            let store = db.store();
            store
                .create_task(NewTask {
                    title: "secret task".into(),
                    body: String::new(),
                    status: Status::ToDo,
                    priority: Priority::Normal,
                    start_date: None,
                    deadline: None,
                })
                .unwrap();
        }

        let db = LockedDb::open(&path, &key_hex, JournalMode::Wal).unwrap();
        let store = db.store();
        let tasks = store.list_tasks(Status::ToDo).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "secret task");
        assert_eq!(tasks[0].board_id, None);
    }

    #[test]
    fn create_fails_if_path_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        std::fs::write(&path, b"anything").unwrap();
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        let err = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap_err();
        assert!(matches!(err, StorageError::Io(_)));
    }

    #[cfg(unix)]
    #[test]
    fn created_encrypted_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("perm.db");
        let _db = LockedDb::create(&path, &"ab".repeat(32), "perm", JournalMode::Wal).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "encrypted board file must not be world-readable");
        let dmode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(dmode & 0o777, 0o700, "data directory must not be world-listable");
    }

    #[test]
    fn locked_db_delete_mode_reports_delete_journal_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Delete).unwrap();
        let mode: String = db.conn.query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap();
        assert_eq!(mode, "delete");
    }

    #[test]
    fn reopening_wal_locked_board_with_delete_converts_and_keeps_rows_no_wal_file_left() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        {
            let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
            db.store()
                .create_task(NewTask {
                    title: "keep me".into(),
                    body: String::new(),
                    status: Status::ToDo,
                    priority: Priority::Normal,
                    start_date: None,
                    deadline: None,
                })
                .unwrap();
        }

        {
            let db = LockedDb::open(&path, &key_hex, JournalMode::Delete).unwrap();
            let mode: String = db.conn.query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap();
            assert_eq!(mode, "delete");
            let tasks = db.store().list_tasks(Status::ToDo).unwrap();
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0].title, "keep me");
        }

        let wal_path = dir.path().join("board.db-wal");
        assert!(!wal_path.exists(), "no -wal file should remain after switching to DELETE mode and closing");
    }

    #[test]
    fn wrong_key_on_open_surfaces_as_wrong_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let right_key = key_hex_for("hunter2", &salt);
        let wrong_key = key_hex_for("wrong", &salt);

        LockedDb::create(&path, &right_key, "vault", JournalMode::Wal).unwrap();
        let err = LockedDb::open(&path, &wrong_key, JournalMode::Wal).unwrap_err();
        assert!(matches!(err, StorageError::WrongPassphrase));
    }

    #[test]
    fn file_is_genuinely_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);

        {
            let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
            db.store()
                .create_task(NewTask {
                    title: "PLAINTEXT_CANARY".into(),
                    body: String::new(),
                    status: Status::ToDo,
                    priority: Priority::Normal,
                    start_date: None,
                    deadline: None,
                })
                .unwrap();
        }

        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3"));
        let as_text = String::from_utf8_lossy(&bytes);
        assert!(!as_text.contains("PLAINTEXT_CANARY"));
    }

    #[test]
    fn locked_store_move_reorder_delete_promote_note_match_plain_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
        let store = db.store();

        let a = store
            .create_task(NewTask { title: "a".into(), body: String::new(), status: Status::ToDo, priority: Priority::Normal, start_date: None, deadline: None })
            .unwrap();
        let b = store
            .create_task(NewTask { title: "b".into(), body: String::new(), status: Status::ToDo, priority: Priority::Normal, start_date: None, deadline: None })
            .unwrap();
        let c = store
            .create_task(NewTask { title: "c".into(), body: String::new(), status: Status::ToDo, priority: Priority::Normal, start_date: None, deadline: None })
            .unwrap();

        store.move_task(a, Status::Doing, 0).unwrap();
        let todo = store.list_tasks(Status::ToDo).unwrap();
        assert_eq!(todo.iter().map(|t| t.id).collect::<Vec<_>>(), vec![b, c]);
        assert_eq!(todo.iter().map(|t| t.position).collect::<Vec<_>>(), vec![0, 1]);

        store.reorder(Status::ToDo, &[c, b]).unwrap();
        let todo = store.list_tasks(Status::ToDo).unwrap();
        assert_eq!(todo.iter().map(|t| t.id).collect::<Vec<_>>(), vec![c, b]);

        store.delete_task(b).unwrap();
        let todo = store.list_tasks(Status::ToDo).unwrap();
        assert_eq!(todo.len(), 1);
        assert_eq!(todo[0].id, c);
        assert_eq!(todo[0].position, 0);

        let note_id = store.create_note("Buy milk\nand eggs").unwrap();
        let task_id = store.promote_note(note_id, Status::ToDo).unwrap();
        let task = store.get_task(task_id).unwrap();
        assert_eq!(task.title, "Buy milk");
        assert_eq!(task.body, "and eggs");
    }

    #[test]
    fn create_read_task_with_and_without_dates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
        shared_tests::create_read_task_with_and_without_dates(&db.store());
    }

    #[test]
    fn clear_deadline_via_patch_leaves_title_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
        shared_tests::clear_deadline_via_patch_leaves_title_untouched(&db.store());
    }

    #[test]
    fn tag_upsert_is_idempotent_and_rejects_empty_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
        shared_tests::tag_upsert_is_idempotent_and_rejects_empty_name(&db.store());
    }

    #[test]
    fn tag_rename_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
        shared_tests::tag_rename_and_delete(&db.store());
    }

    #[test]
    fn set_task_tags_replaces_whole_set_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
        shared_tests::set_task_tags_replaces_whole_set_and_reads_back(&db.store());
    }

    #[test]
    fn deleting_task_cascades_its_tags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
        shared_tests::deleting_task_cascades_its_tags(&db.store());
    }

    #[test]
    fn tags_load_with_list_tasks_for_many_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
        shared_tests::tags_load_with_list_tasks_for_many_tasks(&db.store());
    }

    #[test]
    fn move_task_sets_and_clears_completed_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
        shared_tests::move_task_sets_and_clears_completed_at(&db.store());
    }

    #[test]
    fn create_task_directly_in_done_sets_completed_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let salt = crypto::generate_salt();
        let key_hex = key_hex_for("hunter2", &salt);
        let db = LockedDb::create(&path, &key_hex, "vault", JournalMode::Wal).unwrap();
        shared_tests::create_task_directly_in_done_sets_completed_at(&db.store());
    }
}
