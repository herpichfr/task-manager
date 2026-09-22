pub type BoardId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardKind {
    Plain,
    Locked,
}

impl BoardKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BoardKind::Plain => "plain",
            BoardKind::Locked => "locked",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "plain" => Some(BoardKind::Plain),
            "locked" => Some(BoardKind::Locked),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Board {
    pub id: BoardId,
    pub name: String,
    pub kind: BoardKind,
    pub db_path: Option<String>,
    pub kdf_salt: Option<Vec<u8>>,
    pub kdf_m_cost: Option<u32>,
    pub kdf_t_cost: Option<u32>,
    pub kdf_p_cost: Option<u32>,
    pub position: i64,
    pub created_at: i64,
    pub updated_at: i64,
}
