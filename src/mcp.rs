use std::io::BufRead;

use crate::error::{Result, io};
use crate::store::Environment;

pub fn mcp_server_command(_environment: &Environment) -> Result<i32> {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        line.map_err(|source| io("stdin", source))?;
    }
    Ok(0)
}
