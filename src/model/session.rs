use serde::{Deserialize, Serialize};

use crate::model::{SessionId, WorktreeIdentity};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMeta {
    pub schema_version: u32,
    pub id: SessionId,
    pub created_at: String,
    pub worktree: WorktreeIdentity,
    pub parent_session_id: Option<SessionId>,
    pub parent_checkpoint_sequence: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRef {
    pub schema_version: u32,
    pub key: String,
    pub session_id: SessionId,
    pub identity: WorktreeIdentity,
}
