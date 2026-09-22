use crate::domain::board::BoardId;

pub type TaskId = i64;
pub type TagId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    ToDo,
    Doing,
    Done,
}

impl Status {
    pub const ALL: [Status; 3] = [Status::ToDo, Status::Doing, Status::Done];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::ToDo => "todo",
            Status::Doing => "doing",
            Status::Done => "done",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "todo" => Some(Status::ToDo),
            "doing" => Some(Status::Doing),
            "done" => Some(Status::Done),
            _ => None,
        }
    }

    /// Saturating move toward `Done`, for H/L column moves.
    pub fn next(self) -> Self {
        match self {
            Status::ToDo => Status::Doing,
            Status::Doing => Status::Done,
            Status::Done => Status::Done,
        }
    }

    /// Saturating move toward `ToDo`, for H/L column moves.
    pub fn prev(self) -> Self {
        match self {
            Status::ToDo => Status::ToDo,
            Status::Doing => Status::ToDo,
            Status::Done => Status::Doing,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Urgent,
}

impl Priority {
    pub const ALL: [Priority; 4] = [
        Priority::Low,
        Priority::Normal,
        Priority::High,
        Priority::Urgent,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "low" => Some(Priority::Low),
            "normal" => Some(Priority::Normal),
            "high" => Some(Priority::High),
            "urgent" => Some(Priority::Urgent),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: TaskId,
    pub board_id: Option<BoardId>,
    pub title: String,
    pub body: String,
    pub status: Status,
    pub priority: Priority,
    pub position: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// Unix seconds, `None` = unset.
    pub start_date: Option<i64>,
    /// Unix seconds, `None` = unset.
    pub deadline: Option<i64>,
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone)]
pub struct NewTask {
    pub title: String,
    pub body: String,
    pub status: Status,
    pub priority: Priority,
    /// Unix seconds, `None` = unset.
    pub start_date: Option<i64>,
    /// Unix seconds, `None` = unset.
    pub deadline: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub status: Option<Status>,
    pub priority: Option<Priority>,
    /// `None` = leave unchanged, `Some(None)` = clear, `Some(Some(v))` = set.
    pub start_date: Option<Option<i64>>,
    /// `None` = leave unchanged, `Some(None)` = clear, `Some(Some(v))` = set.
    pub deadline: Option<Option<i64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub id: TagId,
    pub name: String,
    pub color: Option<String>,
}
