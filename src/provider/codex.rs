use std::ffi::OsString;
use std::path::Path;

use crate::error::Result;
use crate::model::Provider;
use crate::provider::{
    LaunchContext, LaunchSpec, ProviderAdapter, base_environment, materialize_immutable,
    probe_version, refresh_symlink, verify_materialized,
};

const HOOKS_JSON: &[u8] = include_bytes!("assets/codex-hooks.json");
const SWITCH_SKILL: &[u8] = include_bytes!("assets/codex-skill-switch.md");

/// The one skill Handover owns. A user skill of the same name is shadowed, not
/// merged — see `link_user_skills`.
const HANDOVER_SKILL: &str = "handover-switch";

/// How many of the user's own skills are linked into a private `CODEX_HOME`.
///
/// The walk runs on every launch, so it is bounded rather than unbounded: a
/// pathological skills directory must make a launch slower, never stop it.
const MAX_LINKED_USER_SKILLS: usize = 256;

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
            &context.integration_root.join("codex/1"),
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
        let version = integration_root.join("codex/1");
        materialize_immutable(&version.join("hooks.json"), HOOKS_JSON)?;
        materialize_immutable(
            &version.join("skills").join(HANDOVER_SKILL).join("SKILL.md"),
            SWITCH_SKILL,
        )
    }

    fn verify(&self, integration_root: &Path) -> Result<()> {
        let version = integration_root.join("codex/1");
        verify_materialized(&version.join("hooks.json"), HOOKS_JSON)?;
        verify_materialized(
            &version.join("skills").join(HANDOVER_SKILL).join("SKILL.md"),
            SWITCH_SKILL,
        )
    }

    fn probe(&self) -> Result<String> {
        probe_version(self.provider())
    }
}

/// Build the private per-run `CODEX_HOME`.
///
/// `integration_version` is `integrations/codex/1` — the directory holding
/// every asset this function links, which is why it is passed whole rather
/// than one path per asset.
pub(crate) fn materialize_codex_home(
    codex_home: &Path,
    integration_version: &Path,
    provider_home: Option<&Path>,
) -> Result<()> {
    crate::store::ensure_private_dir(codex_home)?;
    refresh_symlink(
        &integration_version.join("hooks.json"),
        &codex_home.join("hooks.json"),
    )?;

    // A real directory, not a link: Handover's own skill and the user's must
    // sit side by side, and a symlinked directory cannot hold an added entry.
    let skills = codex_home.join("skills");
    crate::store::ensure_private_dir(&skills)?;
    refresh_symlink(
        &integration_version.join("skills").join(HANDOVER_SKILL),
        &skills.join(HANDOVER_SKILL),
    )?;

    if let Some(real_home) = provider_home {
        for name in ["config.toml", "auth.json"] {
            let source = real_home.join(name);
            if source.exists() {
                refresh_symlink(&source, &codex_home.join(name))?;
            }
        }
        link_user_skills(&real_home.join("skills"), &skills);
    }
    Ok(())
}

/// Link each entry of the user's real `skills/` into the private one.
///
/// Returns nothing on purpose. The user's skills are a convenience; the private
/// home's own assets are what the launched session depends on. Every failure
/// mode here — no `skills/` at all, an unreadable one, an entry that cannot be
/// linked, more entries than the cap — degrades to "fewer skills, one warning".
/// A `Result` would invite a `?` at the call site and turn a cosmetic problem
/// into a failed launch.
///
/// The walk is one level deep and links each entry whole, so a deep tree costs
/// nothing. Entry types come from the directory entry and are never followed,
/// so a dangling symlink is classified rather than erroring — it is simply
/// re-linked, and Codex skips it exactly as it would have in the real home.
///
/// Handover's own `handover-switch` wins a name collision: it is the skill the
/// launched session is instructed to use, and the handover text advertises it.
/// Dot-entries are skipped entirely — `.system` is Codex's own, and it rewrites
/// it into whatever `CODEX_HOME` it is handed.
fn link_user_skills(source: &Path, target: &Path) {
    let entries = match std::fs::read_dir(source) {
        Ok(entries) => entries,
        // No skills directory at all is the ordinary case, not a problem.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            eprintln!(
                "warning: cannot read {} ({error}); your own Codex skills are not available in this session",
                source.display()
            );
            return;
        }
    };

    let mut attempted = 0usize;
    let mut truncated = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        // Codex writes its own built-in skills into `skills/.system` every time
        // it starts, marker file and all. Linking the user's would make Codex
        // refresh them *through* the link, into the user's real `~/.codex` --
        // the one thing the private home exists to prevent. No dot-entry is a
        // user skill, so none of them are linked.
        if name.as_encoded_bytes().first() == Some(&b'.') {
            continue;
        }
        if name == std::ffi::OsStr::new(HANDOVER_SKILL) {
            eprintln!(
                "warning: your own {HANDOVER_SKILL} skill is shadowed by Handover's in this session"
            );
            continue;
        }
        // A skill is a directory. Anything else in there -- README.md,
        // .DS_Store -- is not one and is skipped.
        match entry.file_type() {
            Ok(kind) if kind.is_dir() || kind.is_symlink() => {}
            _ => continue,
        }
        if attempted == MAX_LINKED_USER_SKILLS {
            truncated = true;
            break;
        }
        attempted += 1;
        if let Err(error) = refresh_symlink(&entry.path(), &target.join(&name)) {
            eprintln!(
                "warning: cannot link the Codex skill {} ({error})",
                name.to_string_lossy()
            );
        }
    }

    if truncated {
        eprintln!(
            "warning: linking only the first {MAX_LINKED_USER_SKILLS} of your Codex skills into this session"
        );
    }
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
                hook_bin: Path::new("/usr/local/bin/handover"),
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
                hook_bin: Path::new("/usr/local/bin/handover"),
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
                hook_bin: Path::new("/usr/local/bin/handover"),
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
        let version = integration_root.join("codex/1");
        let codex_home = temp.path().join("codex_home");

        materialize_codex_home(&codex_home, &version, None).unwrap();
        materialize_codex_home(&codex_home, &version, None).unwrap();

        assert_eq!(
            std::fs::read(codex_home.join("hooks.json")).unwrap(),
            std::fs::read(version.join("hooks.json")).unwrap()
        );
        assert_eq!(
            std::fs::read(codex_home.join("skills/handover-switch/SKILL.md")).unwrap(),
            std::fs::read(version.join("skills/handover-switch/SKILL.md")).unwrap()
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
                hook_bin: Path::new("/bin/handover"),
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
        assert!(text.contains("\\\"$HANDOVER_HOOK_BIN\\\" __hook codex"));
        assert!(!text.contains("/work/"));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let hooks_object = value["hooks"].as_object().unwrap();
        assert_eq!(hooks_object.len(), 5);
        for definitions in hooks_object.values() {
            let command = definitions[0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(command, "\"$HANDOVER_HOOK_BIN\" __hook codex");
            assert_eq!(command.matches('$').count(), 1);
        }

        std::fs::write(&hooks, b"different").unwrap();
        assert!(CodexAdapter.setup(temp.path()).is_err());
    }

    #[test]
    fn setup_installs_a_switch_skill_that_arms_through_the_cli() {
        let temp = TempDir::new().unwrap();
        CodexAdapter.setup(temp.path()).unwrap();

        let skill = temp.path().join("codex/1/skills/handover-switch/SKILL.md");
        let text = std::fs::read_to_string(&skill).unwrap();
        assert_eq!(
            std::fs::metadata(&skill).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Codex discovers a skill by its frontmatter, so both keys must be there.
        assert!(text.starts_with("---\n"), "SKILL.md needs YAML frontmatter");
        assert!(text.contains("\nname: handover-switch\n"));
        assert!(text.contains("\ndescription: "));
        assert!(
            text.contains("arm") && text.contains("--from-provider"),
            "the skill must reach arm through the CLI"
        );
        assert!(
            text.contains("checkpoint --format json --from-provider"),
            "the skill must write a narrative checkpoint before arming"
        );
        CodexAdapter.verify(temp.path()).unwrap();

        std::fs::remove_file(&skill).unwrap();
        assert!(CodexAdapter.verify(temp.path()).is_err());
        CodexAdapter.setup(temp.path()).unwrap();
        CodexAdapter.verify(temp.path()).unwrap();
    }

    #[test]
    fn the_private_codex_home_exposes_handovers_skill_through_a_real_skills_directory() {
        let temp = TempDir::new().unwrap();
        let integration_root = temp.path().join("integrations");
        CodexAdapter.setup(&integration_root).unwrap();
        let codex_home = temp.path().join("codex_home");

        materialize_codex_home(&codex_home, &integration_root.join("codex/1"), None).unwrap();

        // The directory itself is real: Task 4 adds the user's own skills into
        // it, and a symlinked directory cannot also hold an added entry.
        let skills = codex_home.join("skills");
        assert!(
            !std::fs::symlink_metadata(&skills)
                .unwrap()
                .file_type()
                .is_symlink(),
            "skills/ must be a real directory"
        );
        let entry = skills.join("handover-switch");
        assert!(
            std::fs::symlink_metadata(&entry)
                .unwrap()
                .file_type()
                .is_symlink(),
            "Handover's own skill is linked, not copied"
        );
        assert_eq!(
            std::fs::read(entry.join("SKILL.md")).unwrap(),
            std::fs::read(integration_root.join("codex/1/skills/handover-switch/SKILL.md"))
                .unwrap()
        );
    }

    /// Build an integration root and a real `skills/` directory holding
    /// `names`, and return `(integration version dir, real codex home)`.
    fn codex_home_fixture(
        temp: &TempDir,
        names: &[&str],
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let integration_root = temp.path().join("integrations");
        CodexAdapter.setup(&integration_root).unwrap();
        let provider_home = temp.path().join("real-codex-home");
        let skills = provider_home.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        for name in names {
            std::fs::create_dir_all(skills.join(name)).unwrap();
            std::fs::write(
                skills.join(name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: user skill\n---\n"),
            )
            .unwrap();
        }
        (integration_root.join("codex/1"), provider_home)
    }

    #[test]
    fn the_users_own_skills_are_linked_beside_handovers() {
        let temp = TempDir::new().unwrap();
        let (version, provider_home) = codex_home_fixture(&temp, &["bokio", "graphify"]);
        let codex_home = temp.path().join("codex_home");

        materialize_codex_home(&codex_home, &version, Some(&provider_home)).unwrap();

        let skills = codex_home.join("skills");
        for name in ["bokio", "graphify"] {
            let text = std::fs::read_to_string(skills.join(name).join("SKILL.md"))
                .unwrap_or_else(|error| panic!("{name} is not reachable: {error}"));
            assert!(text.contains(&format!("name: {name}")));
        }
        // Handover's own is still there beside them.
        assert!(skills.join("handover-switch/SKILL.md").exists());
    }

    #[test]
    fn a_user_skill_named_handover_switch_does_not_shadow_handovers() {
        let temp = TempDir::new().unwrap();
        let (version, provider_home) = codex_home_fixture(&temp, &["handover-switch"]);
        let codex_home = temp.path().join("codex_home");

        materialize_codex_home(&codex_home, &version, Some(&provider_home)).unwrap();

        // Handover's, not the user's: the launched session is told to use this
        // exact skill, so it must be the one it finds.
        let text =
            std::fs::read_to_string(codex_home.join("skills/handover-switch/SKILL.md")).unwrap();
        assert!(
            text.contains("--from-provider"),
            "Handover's skill must win the collision, got: {text}"
        );
        assert!(!text.contains("description: user skill"));
    }

    #[test]
    fn a_hostile_skills_directory_degrades_instead_of_failing_the_launch() {
        let temp = TempDir::new().unwrap();
        let (version, provider_home) = codex_home_fixture(&temp, &["good"]);
        let skills = provider_home.join("skills");
        // A dangling symlink, and a plain file that is not a skill at all.
        std::os::unix::fs::symlink(temp.path().join("nowhere"), skills.join("dangling")).unwrap();
        std::fs::write(skills.join("README.md"), b"not a skill").unwrap();
        let codex_home = temp.path().join("codex_home");

        // The launch must survive all of it.
        materialize_codex_home(&codex_home, &version, Some(&provider_home)).unwrap();

        let private = codex_home.join("skills");
        assert!(
            private.join("good/SKILL.md").exists(),
            "good skills still link"
        );
        assert!(private.join("handover-switch/SKILL.md").exists());
        assert!(
            !private.join("README.md").exists(),
            "a plain file is not a skill and is skipped"
        );
    }

    /// Verified against Codex 0.145: on every start it (re)writes its built-in
    /// skills into `$CODEX_HOME/skills/.system`, marker file and all. If that
    /// entry were a symlink to the user's real one, Codex would write through
    /// it into `~/.codex` — the exact thing the private home exists to prevent.
    #[test]
    fn codexs_own_system_skills_directory_is_never_linked_into_the_private_home() {
        let temp = TempDir::new().unwrap();
        let (version, provider_home) = codex_home_fixture(&temp, &[".system", "mine"]);
        let codex_home = temp.path().join("codex_home");

        materialize_codex_home(&codex_home, &version, Some(&provider_home)).unwrap();

        let private = codex_home.join("skills");
        assert!(
            std::fs::symlink_metadata(private.join(".system")).is_err(),
            "linking .system would let Codex write into the user's real ~/.codex"
        );
        assert!(
            private.join("mine/SKILL.md").exists(),
            "ordinary skills still link"
        );
    }

    #[test]
    fn a_missing_skills_directory_is_the_ordinary_case_and_not_an_error() {
        let temp = TempDir::new().unwrap();
        let integration_root = temp.path().join("integrations");
        CodexAdapter.setup(&integration_root).unwrap();
        let provider_home = temp.path().join("real-codex-home");
        std::fs::create_dir(&provider_home).unwrap();
        let codex_home = temp.path().join("codex_home");

        materialize_codex_home(
            &codex_home,
            &integration_root.join("codex/1"),
            Some(&provider_home),
        )
        .unwrap();

        assert!(codex_home.join("skills/handover-switch/SKILL.md").exists());
    }

    #[test]
    fn more_skills_than_the_cap_are_truncated_rather_than_failing() {
        let temp = TempDir::new().unwrap();
        let names: Vec<String> = (0..super::MAX_LINKED_USER_SKILLS + 8)
            .map(|index| format!("skill-{index:04}"))
            .collect();
        let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
        let (version, provider_home) = codex_home_fixture(&temp, &borrowed);
        let codex_home = temp.path().join("codex_home");

        materialize_codex_home(&codex_home, &version, Some(&provider_home)).unwrap();

        let linked = std::fs::read_dir(codex_home.join("skills"))
            .unwrap()
            .count();
        // Handover's own, plus at most the cap of the user's.
        assert_eq!(linked, super::MAX_LINKED_USER_SKILLS + 1);
    }
}
