use std::ffi::OsString;
use std::path::Path;

use crate::error::Result;
use crate::model::Provider;
use crate::provider::{
    LaunchContext, LaunchSpec, ProviderAdapter, base_environment, materialize_immutable,
    probe_version, refresh_symlink, verify_materialized,
};

const HOOKS_JSON: &[u8] = include_bytes!("assets/codex-hooks.json");

#[derive(Clone, Copy, Debug, Default)]
pub struct CodexAdapter;

impl ProviderAdapter for CodexAdapter {
    fn provider(&self) -> Provider {
        Provider::Codex
    }

    fn launch_spec(&self, context: LaunchContext<'_>) -> Result<LaunchSpec> {
        let codex_home = context.run_dir.join("codex_home");
        materialize_codex_home(
            &codex_home,
            &context.integration_root.join("codex/1/hooks.json"),
            context.provider_home,
        )?;

        let mut args = Vec::with_capacity(context.provider_args.len() + 5);
        args.push(OsString::from("--dangerously-bypass-hook-trust"));
        args.push(OsString::from("--add-dir"));
        args.push(context.inbox.as_os_str().to_owned());
        args.push(OsString::from("-C"));
        args.push(context.cwd.as_os_str().to_owned());
        args.extend(context.provider_args.iter().cloned());
        if let Some(bootstrap) = context.bootstrap {
            args.push(OsString::from(bootstrap));
        }
        let mut env = base_environment(context.hook_bin);
        env.insert(OsString::from("CODEX_HOME"), codex_home.into_os_string());
        Ok(LaunchSpec {
            program: OsString::from(self.provider().executable()),
            args,
            env,
            cwd: context.cwd.to_path_buf(),
        })
    }

    fn setup(&self, integration_root: &Path) -> Result<()> {
        materialize_immutable(&integration_root.join("codex/1/hooks.json"), HOOKS_JSON)
    }

    fn verify(&self, integration_root: &Path) -> Result<()> {
        verify_materialized(&integration_root.join("codex/1/hooks.json"), HOOKS_JSON)
    }

    fn probe(&self) -> Result<String> {
        probe_version(self.provider())
    }
}

pub(crate) fn materialize_codex_home(
    codex_home: &Path,
    hooks_json_asset: &Path,
    provider_home: Option<&Path>,
) -> Result<()> {
    crate::store::ensure_private_dir(codex_home)?;
    refresh_symlink(hooks_json_asset, &codex_home.join("hooks.json"))?;
    if let Some(real_home) = provider_home {
        for name in ["config.toml", "auth.json"] {
            let source = real_home.join(name);
            if source.exists() {
                refresh_symlink(&source, &codex_home.join(name))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use tempfile::TempDir;

    use super::{CodexAdapter, materialize_codex_home};
    use crate::provider::{LaunchContext, ProviderAdapter};

    #[test]
    fn launch_spec_wires_a_private_codex_home_and_bypasses_hook_trust() {
        let temp = TempDir::new().unwrap();
        let integration_root = temp.path().join("integrations");
        CodexAdapter.setup(&integration_root).unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir(&run_dir).unwrap();
        let args = [OsString::from("--model"), OsString::from("test")];

        let spec = CodexAdapter
            .launch_spec(LaunchContext {
                cwd: temp.path(),
                inbox: &temp.path().join("inbox"),
                integration_root: &integration_root,
                hook_bin: Path::new("/usr/local/bin/sesh"),
                provider_args: &args,
                bootstrap: None,
                run_dir: &run_dir,
                provider_home: None,
            })
            .unwrap();

        assert_eq!(spec.program, OsString::from("codex"));
        assert!(
            spec.args
                .iter()
                .any(|arg| arg == "--dangerously-bypass-hook-trust")
        );
        assert!(spec.args.iter().all(|arg| arg != "-c"));
        assert!(spec.args.windows(2).any(|pair| {
            pair == [
                OsString::from("--add-dir"),
                temp.path().join("inbox").into_os_string(),
            ]
        }));
        assert!(
            spec.args
                .windows(2)
                .any(|pair| { pair == [OsString::from("-C"), temp.path().as_os_str().to_owned()] })
        );
        assert!(spec.args.ends_with(&args));

        let codex_home = run_dir.join("codex_home");
        assert_eq!(
            spec.env.get(&OsString::from("CODEX_HOME")),
            Some(&codex_home.clone().into_os_string())
        );
        let hooks_json = codex_home.join("hooks.json");
        assert!(
            std::fs::symlink_metadata(&hooks_json)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(&hooks_json).unwrap(),
            std::fs::read(integration_root.join("codex/1/hooks.json")).unwrap()
        );
    }

    #[test]
    fn launch_spec_symlinks_the_real_provider_home_files_when_present() {
        let temp = TempDir::new().unwrap();
        let integration_root = temp.path().join("integrations");
        CodexAdapter.setup(&integration_root).unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir(&run_dir).unwrap();
        let provider_home = temp.path().join("real-codex-home");
        std::fs::create_dir(&provider_home).unwrap();
        std::fs::write(provider_home.join("config.toml"), b"model = \"test\"\n").unwrap();
        std::fs::write(provider_home.join("auth.json"), b"{}").unwrap();

        CodexAdapter
            .launch_spec(LaunchContext {
                cwd: temp.path(),
                inbox: &temp.path().join("inbox"),
                integration_root: &integration_root,
                hook_bin: Path::new("/usr/local/bin/sesh"),
                provider_args: &[],
                bootstrap: None,
                run_dir: &run_dir,
                provider_home: Some(&provider_home),
            })
            .unwrap();

        let codex_home = run_dir.join("codex_home");
        for name in ["config.toml", "auth.json"] {
            let link = codex_home.join(name);
            assert!(
                std::fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                std::fs::read(&link).unwrap(),
                std::fs::read(provider_home.join(name)).unwrap()
            );
        }
    }

    #[test]
    fn launch_spec_skips_missing_provider_home_files_without_erroring() {
        let temp = TempDir::new().unwrap();
        let integration_root = temp.path().join("integrations");
        CodexAdapter.setup(&integration_root).unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir(&run_dir).unwrap();
        let empty_provider_home = temp.path().join("no-such-codex-home-contents");
        std::fs::create_dir(&empty_provider_home).unwrap();

        let spec = CodexAdapter
            .launch_spec(LaunchContext {
                cwd: temp.path(),
                inbox: &temp.path().join("inbox"),
                integration_root: &integration_root,
                hook_bin: Path::new("/usr/local/bin/sesh"),
                provider_args: &[],
                bootstrap: None,
                run_dir: &run_dir,
                provider_home: Some(&empty_provider_home),
            })
            .unwrap();

        assert!(spec.env.contains_key(&OsString::from("CODEX_HOME")));
        let codex_home = run_dir.join("codex_home");
        assert!(!codex_home.join("config.toml").exists());
        assert!(!codex_home.join("auth.json").exists());
    }

    #[test]
    fn materialize_codex_home_is_idempotent_and_refreshes_stale_symlinks() {
        let temp = TempDir::new().unwrap();
        let integration_root = temp.path().join("integrations");
        CodexAdapter.setup(&integration_root).unwrap();
        let hooks_asset = integration_root.join("codex/1/hooks.json");
        let codex_home = temp.path().join("codex_home");

        materialize_codex_home(&codex_home, &hooks_asset, None).unwrap();
        materialize_codex_home(&codex_home, &hooks_asset, None).unwrap();

        assert_eq!(
            std::fs::read(codex_home.join("hooks.json")).unwrap(),
            std::fs::read(&hooks_asset).unwrap()
        );
    }

    #[test]
    fn bootstrap_is_optional_and_raw_unix_arguments_are_preserved() {
        let temp = TempDir::new().unwrap();
        let integration_root = temp.path().join("integrations");
        CodexAdapter.setup(&integration_root).unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir(&run_dir).unwrap();
        let raw = OsString::from_vec(vec![b'-', b'-', b'x', 0xff]);
        let args = [raw.clone()];
        let spec = CodexAdapter
            .launch_spec(LaunchContext {
                cwd: temp.path(),
                inbox: &temp.path().join("inbox"),
                integration_root: &integration_root,
                hook_bin: Path::new("/bin/sesh"),
                provider_args: &args,
                bootstrap: Some("continue"),
                run_dir: &run_dir,
                provider_home: None,
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
        let hooks = temp.path().join("codex/1/hooks.json");
        assert_eq!(
            std::fs::metadata(&hooks).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let text = std::fs::read_to_string(&hooks).unwrap();
        assert!(text.contains("\\\"$SESH_HOOK_BIN\\\" __hook codex"));
        assert!(!text.contains("/work/"));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let hooks_object = value["hooks"].as_object().unwrap();
        assert_eq!(hooks_object.len(), 5);
        for definitions in hooks_object.values() {
            let command = definitions[0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(command, "\"$SESH_HOOK_BIN\" __hook codex");
            assert_eq!(command.matches('$').count(), 1);
        }

        std::fs::write(&hooks, b"different").unwrap();
        assert!(CodexAdapter.setup(temp.path()).is_err());
    }
}
