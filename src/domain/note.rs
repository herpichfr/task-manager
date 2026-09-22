use crate::domain::board::BoardId;
use crate::domain::task::TaskId;

pub type NoteId = i64;

#[derive(Debug, Clone)]
pub struct Note {
    pub id: NoteId,
    pub board_id: Option<BoardId>,
    pub body: String,
    pub promoted_task_id: Option<TaskId>,
    pub created_at: i64,
    pub updated_at: i64,
}
