use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result, io};
use crate::model::Provider;
use crate::store::Environment;

/// What opening a provider's desktop application costs: one command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopLaunch {
    pub program: OsString,
    pub args: Vec<OsString>,
}

impl DesktopLaunch {
    /// The command as a user would type it, for the line that reports what was
    /// opened — and for the one that reports what could not be.
    pub fn describe(&self) -> String {
        shell_words::join(
            std::iter::once(self.program.as_os_str())
                .chain(self.args.iter().map(OsString::as_os_str))
                .map(|word| word.to_string_lossy()),
        )
    }
}

/// The command that opens `provider`'s desktop application on this worktree.
///
/// Neither entry point accepts what Handover injects at a CLI launch — no
/// plugin directory, no `CODEX_HOME`, no hook binary, no run inbox. That
/// limitation is what forces the target to pull its handover over MCP, and what
/// makes a desktop session attach tier.
pub fn desktop_launch(provider: Provider, worktree: &Path) -> DesktopLaunch {
    match provider {
        // `codex app [PATH]` is an official subcommand: "Launch the Desktop app
        // (opens the app installer if missing)", taking a workspace path and
        // nothing else.
        Provider::Codex => DesktopLaunch {
            program: OsString::from("codex"),
            args: vec![OsString::from("app"), worktree.as_os_str().to_owned()],
        },
        // Claude has no `app` subcommand. `Claude.app` registers the `claude`
        // URL scheme and `claude://code/new` is a route inside its bundle —
        // undocumented private surface, used best-effort. It accepts no
        // workspace path, so none is passed.
        Provider::Claude => DesktopLaunch {
            program: OsString::from("open"),
            args: vec![OsString::from("claude://code/new")],
        },
    }
}

/// How a desktop launch actually happens.
///
/// A trait so tests can assert the command that *would* run: no test may open a
/// real application.
pub trait DesktopLauncher {
    fn launch(&self, spec: &DesktopLaunch) -> Result<()>;
}

/// Spawns and forgets.
///
/// A desktop application outlives the `handover` process, so this must never
/// wait on it, and its stdio goes to null so a GUI launcher cannot write over
/// the terminal the user is still reading.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpawnLauncher;

impl DesktopLauncher for SpawnLauncher {
    fn launch(&self, spec: &DesktopLaunch) -> Result<()> {
        std::process::Command::new(&spec.program)
            .args(&spec.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_child| ())
            .map_err(|error| {
                Error::Command(format!(
                    "cannot open {}: {error}",
                    spec.program.to_string_lossy()
                ))
            })
    }
}

/// The environment variable that redirects a desktop launch into a file.
///
/// Set by tests and by nothing else. An integration test drives the real binary
/// as a child process, so the trait above cannot reach it — a Rust seam does
/// not cross a process boundary — and no test may open a real application. The
/// `HANDOVER_TEST_` prefix says what it is; `tests/repository_contract.rs`
/// asserts it is never documented as a variable a user should set.
pub const TEST_LAUNCH_LOG_ENV: &str = "HANDOVER_TEST_DESKTOP_LAUNCH_LOG";

/// Appends what would have been opened to a file, and opens nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureLauncher {
    log: PathBuf,
}

impl CaptureLauncher {
    pub fn new(log: impl Into<PathBuf>) -> Self {
        Self { log: log.into() }
    }
}

impl DesktopLauncher for CaptureLauncher {
    fn launch(&self, spec: &DesktopLaunch) -> Result<()> {
        use std::io::Write as _;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .map_err(|source| io(&self.log, source))?;
        writeln!(file, "{}", spec.describe()).map_err(|source| io(&self.log, source))
    }
}

/// Which of the two a process uses.
///
/// An enum rather than a boxed trait object so a unit test can assert the
/// *selection* — including that an ordinary environment selects the real
/// launcher — without calling `launch` on it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentLauncher {
    Spawn(SpawnLauncher),
    Capture(CaptureLauncher),
}

impl DesktopLauncher for EnvironmentLauncher {
    fn launch(&self, spec: &DesktopLaunch) -> Result<()> {
        match self {
            Self::Spawn(launcher) => launcher.launch(spec),
            Self::Capture(launcher) => launcher.launch(spec),
        }
    }
}

/// The launcher `environment` asks for: the real one unless a test redirected
/// it.
pub fn launcher_from(environment: &Environment) -> EnvironmentLauncher {
    match environment.get(TEST_LAUNCH_LOG_ENV) {
        Some(log) if !log.is_empty() => EnvironmentLauncher::Capture(CaptureLauncher::new(log)),
        _ => EnvironmentLauncher::Spawn(SpawnLauncher),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::Path;

    use tempfile::TempDir;

    use super::{
        CaptureLauncher, DesktopLaunch, DesktopLauncher, EnvironmentLauncher, Provider,
        SpawnLauncher, TEST_LAUNCH_LOG_ENV, desktop_launch, launcher_from,
    };
    use crate::error::Result;
    use crate::store::Environment;

    /// Records instead of launching, so the suite never opens an application.
    #[derive(Default)]
    struct RecordingLauncher {
        launched: std::cell::RefCell<Vec<DesktopLaunch>>,
    }

    impl DesktopLauncher for RecordingLauncher {
        fn launch(&self, spec: &DesktopLaunch) -> Result<()> {
            self.launched.borrow_mut().push(spec.clone());
            Ok(())
        }
    }

    #[test]
    fn codex_opens_the_desktop_app_on_the_worktree() {
        let spec = desktop_launch(Provider::Codex, Path::new("/work/oauth"));
        assert_eq!(spec.program, "codex");
        assert_eq!(spec.args, ["app", "/work/oauth"]);
    }

    #[test]
    fn claude_opens_its_url_scheme_and_carries_no_worktree() {
        // The route accepts no workspace path. Passing one would invent surface
        // that does not exist — and that absence is exactly why a Claude
        // desktop target has to pull its handover over MCP.
        let spec = desktop_launch(Provider::Claude, Path::new("/work/oauth"));
        assert_eq!(spec.program, "open");
        assert_eq!(spec.args, ["claude://code/new"]);
        assert!(
            !spec
                .args
                .iter()
                .any(|arg| arg.to_string_lossy().contains("/work/oauth")),
            "claude://code/new takes no workspace path"
        );
    }

    #[test]
    fn a_launcher_receives_exactly_the_spec_it_was_given() {
        let launcher = RecordingLauncher::default();
        let spec = desktop_launch(Provider::Codex, Path::new("/work/oauth"));
        launcher.launch(&spec).unwrap();
        assert_eq!(launcher.launched.borrow().as_slice(), [spec]);
    }

    #[test]
    fn a_spec_describes_itself_as_the_command_a_user_would_type() {
        assert_eq!(
            desktop_launch(Provider::Codex, Path::new("/work/oauth")).describe(),
            "codex app /work/oauth"
        );
        assert_eq!(
            desktop_launch(Provider::Claude, Path::new("/work/oauth")).describe(),
            "open claude://code/new"
        );
        // A path with a space stays one word, so the command it prints is the
        // command the user can paste.
        assert_eq!(
            desktop_launch(Provider::Codex, Path::new("/work/my oauth")).describe(),
            "codex app '/work/my oauth'"
        );
    }

    #[test]
    fn an_ordinary_environment_selects_the_real_launcher() {
        // Asserted by construction, never by calling `launch`: doing that here
        // would open an application.
        for environment in [
            Environment::from_pairs(HashMap::new()),
            Environment::from_pairs(HashMap::from([(TEST_LAUNCH_LOG_ENV, OsString::new())])),
        ] {
            assert_eq!(
                launcher_from(&environment),
                EnvironmentLauncher::Spawn(SpawnLauncher)
            );
        }
    }

    #[test]
    fn a_test_can_redirect_a_launch_into_a_file_and_open_nothing() {
        let temp = TempDir::new().unwrap();
        let log = temp.path().join("launches");
        let environment =
            Environment::from_pairs(HashMap::from([(TEST_LAUNCH_LOG_ENV, OsString::from(&log))]));

        let launcher = launcher_from(&environment);
        assert_eq!(
            launcher,
            EnvironmentLauncher::Capture(CaptureLauncher::new(&log))
        );

        launcher
            .launch(&desktop_launch(Provider::Codex, Path::new("/work/oauth")))
            .unwrap();
        launcher
            .launch(&desktop_launch(Provider::Claude, Path::new("/work/oauth")))
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "codex app /work/oauth\nopen claude://code/new\n"
        );
    }
}
