use std::ffi::OsString;
use std::path::Path;

use crate::error::Result;
use crate::model::Provider;
use crate::provider::{
    LaunchContext, LaunchSpec, ProviderAdapter, base_environment, materialize_immutable,
    probe_version, verify_materialized,
};

const PLUGIN_JSON: &[u8] = include_bytes!("assets/claude-plugin.json");
const HOOKS_JSON: &[u8] = include_bytes!("assets/claude-hooks.json");
const CHECKPOINT_COMMAND: &[u8] = include_bytes!("assets/claude-command-checkpoint.md");
const SWITCH_COMMAND: &[u8] = include_bytes!("assets/claude-command-switch.md");

#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudeAdapter;

impl ProviderAdapter for ClaudeAdapter {
    fn provider(&self) -> Provider {
        Provider::Claude
    }

    fn launch_spec(&self, context: LaunchContext<'_>) -> Result<LaunchSpec> {
        let plugin_dir = context.integration_root.join("claude/1");
        let mut args = vec![
            OsString::from("--plugin-dir"),
            plugin_dir.into_os_string(),
            OsString::from("--add-dir"),
            context.inbox.as_os_str().to_owned(),
        ];
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
        let version = integration_root.join("claude/1");
        materialize_immutable(&version.join(".claude-plugin/plugin.json"), PLUGIN_JSON)?;
        materialize_immutable(&version.join("hooks/hooks.json"), HOOKS_JSON)?;
        materialize_immutable(
            &version.join("commands/handover-checkpoint.md"),
            CHECKPOINT_COMMAND,
        )?;
        materialize_immutable(&version.join("commands/handover-switch.md"), SWITCH_COMMAND)
    }

    fn verify(&self, integration_root: &Path) -> Result<()> {
        let version = integration_root.join("claude/1");
        verify_materialized(&version.join(".claude-plugin/plugin.json"), PLUGIN_JSON)?;
        verify_materialized(&version.join("hooks/hooks.json"), HOOKS_JSON)?;
        verify_materialized(
            &version.join("commands/handover-checkpoint.md"),
            CHECKPOINT_COMMAND,
        )?;
        verify_materialized(&version.join("commands/handover-switch.md"), SWITCH_COMMAND)
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

    use super::ClaudeAdapter;
    use crate::provider::{LaunchContext, ProviderAdapter};

    #[test]
    fn launch_spec_is_session_scoped_and_preserves_provider_flags() {
        let args = [OsString::from("--model"), OsString::from("test")];
        let spec = ClaudeAdapter
            .launch_spec(LaunchContext {
                cwd: Path::new("/work/oauth/apps/web"),
                inbox: Path::new("/state/run/inbox"),
                integration_root: Path::new("/state/integrations"),
                hook_bin: Path::new("/usr/local/bin/handover"),
                provider_args: &args,
                bootstrap: None,
                run_dir: Path::new("/state/run"),
                provider_home: None,
            })
            .unwrap();

        assert_eq!(spec.program, OsString::from("claude"));
        assert!(spec.args.windows(2).any(|pair| {
            pair == [
                OsString::from("--plugin-dir"),
                OsString::from("/state/integrations/claude/1"),
            ]
        }));
        assert!(spec.args.windows(2).any(|pair| {
            pair == [
                OsString::from("--add-dir"),
                OsString::from("/state/run/inbox"),
            ]
        }));
        assert!(spec.args.ends_with(&args));
        assert_eq!(spec.cwd, Path::new("/work/oauth/apps/web"));
        assert_eq!(
            spec.env.get(&OsString::from("HANDOVER_HOOK_BIN")),
            Some(&OsString::from("/usr/local/bin/handover"))
        );
        assert_no_session_content(&spec.args);
    }

    #[test]
    fn bootstrap_is_optional_and_raw_unix_arguments_are_preserved() {
        let raw = OsString::from_vec(vec![b'-', b'-', b'x', 0xff]);
        let args = [raw.clone()];
        let spec = ClaudeAdapter
            .launch_spec(LaunchContext {
                cwd: Path::new("/work"),
                inbox: Path::new("/inbox"),
                integration_root: Path::new("/integrations"),
                hook_bin: Path::new("/bin/handover"),
                provider_args: &args,
                bootstrap: Some("continue"),
                run_dir: Path::new("/run"),
                provider_home: None,
            })
            .unwrap();
        assert_eq!(spec.args[spec.args.len() - 2], raw);
        assert_eq!(spec.args.last(), Some(&OsString::from("continue")));
    }

    #[test]
    fn setup_is_private_idempotent_and_refuses_asset_drift() {
        let temp = TempDir::new().unwrap();
        ClaudeAdapter.setup(temp.path()).unwrap();
        ClaudeAdapter.setup(temp.path()).unwrap();
        let plugin = temp.path().join("claude/1/.claude-plugin/plugin.json");
        let hooks = temp.path().join("claude/1/hooks/hooks.json");
        for path in [&plugin, &hooks] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let hooks_text = std::fs::read_to_string(&hooks).unwrap();
        assert!(hooks_text.contains("\\\"$HANDOVER_HOOK_BIN\\\" __hook claude"));
        assert!(!hooks_text.contains("/work/"));
        let hooks_value: serde_json::Value = serde_json::from_str(&hooks_text).unwrap();
        let hooks_object = hooks_value["hooks"].as_object().unwrap();
        assert_eq!(hooks_object.len(), 6);
        for definitions in hooks_object.values() {
            let command = definitions[0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(command, "\"$HANDOVER_HOOK_BIN\" __hook claude");
            assert_eq!(command.matches('$').count(), 1);
        }

        std::fs::write(&plugin, b"different").unwrap();
        assert!(ClaudeAdapter.setup(temp.path()).is_err());
    }

    #[test]
    fn setup_installs_a_checkpoint_command_the_agent_can_invoke() {
        let temp = TempDir::new().unwrap();
        ClaudeAdapter.setup(temp.path()).unwrap();

        let command = temp.path().join("claude/1/commands/handover-checkpoint.md");
        let text = std::fs::read_to_string(&command).unwrap();
        assert_eq!(
            std::fs::metadata(&command).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            text.contains("--from-provider"),
            "an attached provider may only checkpoint through that flag"
        );
        assert!(
            text.contains("objective"),
            "the required fields must appear"
        );
        ClaudeAdapter.verify(temp.path()).unwrap();
    }

    #[test]
    fn verify_rejects_an_install_predating_the_checkpoint_command() {
        // What an install from an earlier Handover looks like: every asset it
        // knew about is present and correct, and the newer one is simply absent.
        let temp = TempDir::new().unwrap();
        ClaudeAdapter.setup(temp.path()).unwrap();
        std::fs::remove_file(temp.path().join("claude/1/commands/handover-checkpoint.md")).unwrap();

        assert!(ClaudeAdapter.verify(temp.path()).is_err());
        // Re-running setup has to restore it, since that is the advice doctor gives.
        ClaudeAdapter.setup(temp.path()).unwrap();
        ClaudeAdapter.verify(temp.path()).unwrap();
    }

    #[test]
    fn setup_installs_a_switch_command_that_arms_through_the_cli() {
        let temp = TempDir::new().unwrap();
        ClaudeAdapter.setup(temp.path()).unwrap();

        let command = temp.path().join("claude/1/commands/handover-switch.md");
        let text = std::fs::read_to_string(&command).unwrap();
        assert_eq!(
            std::fs::metadata(&command).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // The CLI, not MCP: Handover does not register its MCP server, so a
        // command that assumed one would fail on most installs.
        assert!(
            text.contains("arm") && text.contains("--from-provider"),
            "the command must reach arm through the CLI"
        );
        assert!(
            !text.contains("mcp"),
            "the command must not assume an MCP server is configured"
        );
        // A checkpoint first, or the handover the target receives is thin.
        assert!(
            text.contains("checkpoint --format json --from-provider"),
            "the command must write a narrative checkpoint before arming"
        );
        ClaudeAdapter.verify(temp.path()).unwrap();

        // An install predating this asset is outdated, not corrupt: re-running
        // setup restores it, which is the advice doctor gives.
        std::fs::remove_file(&command).unwrap();
        assert!(ClaudeAdapter.verify(temp.path()).is_err());
        ClaudeAdapter.setup(temp.path()).unwrap();
        ClaudeAdapter.verify(temp.path()).unwrap();
    }

    fn assert_no_session_content(args: &[OsString]) {
        let joined = args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        for forbidden in ["transcript", "session_id", "11111111", "OAuth prompt"] {
            assert!(!joined.contains(forbidden), "found {forbidden} in {joined}");
        }
    }
}
