use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Error, Result};
use crate::model::{RunId, SessionId};

pub trait Runtime: Send + Sync {
    fn now(&self) -> Result<String>;
    fn session_id(&self) -> SessionId;
    fn run_id(&self) -> RunId;
}

#[derive(Debug, Default)]
pub struct SystemRuntime;

impl Runtime for SystemRuntime {
    fn now(&self) -> Result<String> {
        OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| Error::InvalidState(format!("cannot format UTC time: {error}")))
    }

    fn session_id(&self) -> SessionId {
        SessionId::new()
    }

    fn run_id(&self) -> RunId {
        RunId::new()
    }
}
