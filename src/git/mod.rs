pub(crate) mod command;
pub mod fork;
mod observe;

use std::path::Path;

use crate::error::Result;
use crate::git::fork::{ForkPreflight, ForkRequest};
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

    pub fn preflight_fork(
        &self,
        source_cwd: &Path,
        caller_cwd: &Path,
        request: &ForkRequest,
        operation_id: &str,
    ) -> Result<ForkPreflight> {
        fork::preflight(&self.command, source_cwd, caller_cwd, request, operation_id)
    }
}
