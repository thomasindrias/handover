pub(crate) mod command;
mod observe;

use std::path::Path;

use crate::error::Result;
use crate::model::GitSnapshot;

#[derive(Clone, Debug, Default)]
pub struct Git {
    command: command::GitCommand,
}

impl Git {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self, cwd: &Path) -> Result<GitSnapshot> {
        observe::snapshot(&self.command, cwd)
    }
}
