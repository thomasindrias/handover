use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, ExitStatus};

use crate::error::{Error, Result};

#[derive(Debug)]
pub struct GitOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct GitCommand;

impl GitCommand {
    pub fn output<I, S>(&self, cwd: &Path, args: I) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args = collect_args(args);
        let output = self.run(cwd, &args)?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(command_failed(&args, &output))
        }
    }

    pub fn text<I, S>(&self, cwd: &Path, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        decode_text(self.output(cwd, args)?)
    }

    pub fn optional_text_exit_one<I, S>(&self, cwd: &Path, args: I) -> Result<Option<String>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args = collect_args(args);
        let output = self.run(cwd, &args)?;
        if output.status.success() {
            decode_text(output.stdout).map(Some)
        } else if output.status.code() == Some(1) {
            Ok(None)
        } else {
            Err(command_failed(&args, &output))
        }
    }

    fn run(&self, cwd: &Path, args: &[OsString]) -> Result<GitOutput> {
        let output = Command::new("git")
            .arg("--no-pager")
            .arg("--literal-pathspecs")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .output()
            .map_err(|error| Error::Command(format!("cannot run git: {error}")))?;
        Ok(GitOutput {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

fn collect_args<I, S>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    args.into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect()
}

fn decode_text(mut bytes: Vec<u8>) -> Result<String> {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map_err(|error| Error::Command(format!("git emitted non-UTF-8 metadata: {error}")))
}

fn command_failed(args: &[OsString], output: &GitOutput) -> Error {
    Error::Command(format!(
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim_end()
    ))
}
