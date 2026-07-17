use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};
use crate::store::atomic::{create_private, read_private, replace_private};

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    replace_private(path, &encode_json(path, value)?)
}

pub fn write_json_create<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    create_private(path, &encode_json(path, value)?)
}

pub fn read_json<T>(path: &Path) -> Result<T>
where
    T: DeserializeOwned + Serialize,
{
    let bytes = read_private(path)?;
    let value: T = serde_json::from_slice(&bytes).map_err(|error| {
        Error::InvalidState(format!("cannot decode {}: {error}", path.display()))
    })?;
    if encode_json(path, &value)? != bytes {
        return Err(Error::InvalidState(format!(
            "{} is not canonical JSON",
            path.display()
        )));
    }
    Ok(value)
}

fn encode_json<T: Serialize>(path: &Path, value: &T) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        Error::InvalidState(format!("cannot encode {}: {error}", path.display()))
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}
