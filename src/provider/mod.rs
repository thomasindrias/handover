pub mod claude;
pub mod codex;
pub mod hook;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::model::Provider;
use crate::store::atomic::{create_private, read_private};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchContext<'a> {
    pub cwd: &'a Path,
    pub inbox: &'a Path,
    pub integration_root: &'a Path,
    pub hook_bin: &'a Path,
    pub provider_args: &'a [OsString],
    pub bootstrap: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: BTreeMap<OsString, OsString>,
    pub cwd: PathBuf,
}

pub trait ProviderAdapter: Send + Sync {
    fn provider(&self) -> Provider;
    fn launch_spec(&self, context: LaunchContext<'_>) -> Result<LaunchSpec>;
    fn setup(&self, integration_root: &Path) -> Result<()>;
    fn probe(&self) -> Result<String>;
}

pub fn adapter(provider: Provider) -> Box<dyn ProviderAdapter> {
    match provider {
        Provider::Claude => Box::new(claude::ClaudeAdapter),
        Provider::Codex => Box::new(codex::CodexAdapter),
    }
}

fn base_environment(hook_bin: &Path) -> BTreeMap<OsString, OsString> {
    BTreeMap::from([(
        OsString::from("SESH_HOOK_BIN"),
        hook_bin.as_os_str().to_owned(),
    )])
}

fn materialize_immutable(path: &Path, expected: &[u8]) -> Result<()> {
    match create_private(path, expected) {
        Ok(()) => Ok(()),
        Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            let actual = read_private(path)?;
            let actual_hash = Sha256::digest(&actual);
            let expected_hash = Sha256::digest(expected);
            if actual.len() == expected.len() && actual_hash == expected_hash {
                Ok(())
            } else {
                Err(Error::InvalidState(format!(
                    "immutable provider asset {} does not match this Sesh version",
                    path.display()
                )))
            }
        }
        Err(error) => Err(error),
    }
}

fn probe_version(provider: Provider) -> Result<String> {
    let output = std::process::Command::new(provider.executable())
        .arg("--version")
        .output()
        .map_err(|error| {
            Error::Command(format!(
                "cannot run {} --version: {error}",
                provider.executable()
            ))
        })?;
    if !output.status.success() {
        return Err(Error::Command(format!(
            "{} --version exited with {}: {}",
            provider.executable(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let version = std::str::from_utf8(&output.stdout)
        .map_err(|_| {
            Error::Command(format!(
                "{} --version returned non-UTF-8 output",
                provider.executable()
            ))
        })?
        .trim();
    if version.is_empty() {
        return Err(Error::Command(format!(
            "{} --version returned no version",
            provider.executable()
        )));
    }
    Ok(version.to_owned())
}
