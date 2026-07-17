use std::ffi::OsString;
use std::path::Path;

use crate::error::Result;
use crate::model::Provider;
use crate::provider::{
    LaunchContext, LaunchSpec, ProviderAdapter, base_environment, materialize_immutable,
    probe_version,
};

const OVERLAYS: [&str; 5] = [
    r#"hooks.SessionStart=[{matcher="startup|resume|clear|compact",hooks=[{type="command",command="\"$SESH_HOOK_BIN\" __hook codex",timeout=30}]}]"#,
    r#"hooks.UserPromptSubmit=[{hooks=[{type="command",command="\"$SESH_HOOK_BIN\" __hook codex",timeout=30}]}]"#,
    r#"hooks.PreToolUse=[{hooks=[{type="command",command="\"$SESH_HOOK_BIN\" __hook codex",timeout=30}]}]"#,
    r#"hooks.PostToolUse=[{hooks=[{type="command",command="\"$SESH_HOOK_BIN\" __hook codex",timeout=120}]}]"#,
    r#"hooks.Stop=[{hooks=[{type="command",command="\"$SESH_HOOK_BIN\" __hook codex",timeout=120}]}]"#,
];

#[derive(Clone, Copy, Debug, Default)]
pub struct CodexAdapter;

impl ProviderAdapter for CodexAdapter {
    fn provider(&self) -> Provider {
        Provider::Codex
    }

    fn launch_spec(&self, context: LaunchContext<'_>) -> Result<LaunchSpec> {
        let mut args = Vec::with_capacity(OVERLAYS.len() * 2 + context.provider_args.len() + 5);
        for overlay in OVERLAYS {
            args.push(OsString::from("-c"));
            args.push(OsString::from(overlay));
        }
        args.push(OsString::from("--add-dir"));
        args.push(context.inbox.as_os_str().to_owned());
        args.push(OsString::from("-C"));
        args.push(context.cwd.as_os_str().to_owned());
        args.extend(context.provider_args.iter().cloned());
        if let Some(bootstrap) = context.bootstrap {
            args.push(OsString::from(bootstrap));
        }
        Ok(LaunchSpec {
            program: OsString::from(self.provider().executable()),
            args,
            env: base_environment(context.hook_bin),
            cwd: context.cwd.to_path_buf(),
        })
    }

    fn setup(&self, integration_root: &Path) -> Result<()> {
        let mut contents = OVERLAYS.join("\n").into_bytes();
        contents.push(b'\n');
        materialize_immutable(&integration_root.join("codex/1/hooks.txt"), &contents)
    }

    fn probe(&self) -> Result<String> {
        probe_version(self.provider())
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use tempfile::TempDir;

    use super::CodexAdapter;
    use crate::provider::{LaunchContext, ProviderAdapter};

    #[test]
    fn launch_spec_is_session_scoped_and_preserves_provider_flags() {
        let args = [OsString::from("--model"), OsString::from("test")];
        let spec = CodexAdapter
            .launch_spec(LaunchContext {
                cwd: Path::new("/work/oauth/apps/web"),
                inbox: Path::new("/state/run/inbox"),
                integration_root: Path::new("/state/integrations"),
                hook_bin: Path::new("/usr/local/bin/sesh"),
                provider_args: &args,
                bootstrap: None,
            })
            .unwrap();

        assert_eq!(spec.program, OsString::from("codex"));
        assert!(
            spec.args
                .iter()
                .any(|arg| arg.to_string_lossy().contains("hooks.SessionStart"))
        );
        assert!(spec.args.windows(2).any(|pair| {
            pair == [
                OsString::from("--add-dir"),
                OsString::from("/state/run/inbox"),
            ]
        }));
        assert!(spec.args.windows(2).any(|pair| {
            pair == [OsString::from("-C"), OsString::from("/work/oauth/apps/web")]
        }));
        assert!(spec.args.ends_with(&args));
        assert_eq!(
            spec.env.get(&OsString::from("SESH_HOOK_BIN")),
            Some(&OsString::from("/usr/local/bin/sesh"))
        );
        let joined = spec
            .args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        for forbidden in ["transcript", "session_id", "11111111", "OAuth prompt"] {
            assert!(!joined.contains(forbidden), "found {forbidden} in {joined}");
        }
    }

    #[test]
    fn bootstrap_is_optional_and_raw_unix_arguments_are_preserved() {
        let raw = OsString::from_vec(vec![b'-', b'-', b'x', 0xff]);
        let args = [raw.clone()];
        let spec = CodexAdapter
            .launch_spec(LaunchContext {
                cwd: Path::new("/work"),
                inbox: Path::new("/inbox"),
                integration_root: Path::new("/integrations"),
                hook_bin: Path::new("/bin/sesh"),
                provider_args: &args,
                bootstrap: Some("continue"),
            })
            .unwrap();
        assert_eq!(spec.args[spec.args.len() - 2], raw);
        assert_eq!(spec.args.last(), Some(&OsString::from("continue")));
    }

    #[test]
    fn setup_is_private_idempotent_and_refuses_asset_drift() {
        let temp = TempDir::new().unwrap();
        CodexAdapter.setup(temp.path()).unwrap();
        CodexAdapter.setup(temp.path()).unwrap();
        let hooks = temp.path().join("codex/1/hooks.txt");
        assert_eq!(
            std::fs::metadata(&hooks).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let text = std::fs::read_to_string(&hooks).unwrap();
        assert_eq!(text.lines().count(), 5);
        assert_eq!(text, format!("{}\n", super::OVERLAYS.join("\n")));
        assert!(text.lines().all(|line| {
            line.contains("\\\"$SESH_HOOK_BIN\\\" __hook codex")
                && !line.contains("/work/")
                && !line.contains("session_id")
                && line.matches('$').count() == 1
        }));

        std::fs::write(&hooks, b"different").unwrap();
        assert!(CodexAdapter.setup(temp.path()).is_err());
    }
}
