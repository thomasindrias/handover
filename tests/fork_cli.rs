mod support;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::TempDir;

use handover::git::fork::default_target;
use handover::model::{Provider, RunId, SessionId};
use handover::store::lease::{ProcessIdentity, RunLease};
use support::{add_linked_worktree, git, init_repo, path_with, write_executable};

#[test]
fn fork_is_a_discoverable_git_like_command() {
    cargo_bin_cmd!("handover")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("fork"));

    cargo_bin_cmd!("handover")
        .args(["fork", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("<PROVIDER>"))
        .stdout(predicate::str::contains("--branch <BRANCH>"))
        .stdout(predicate::str::contains("--worktree <WORKTREE>"));
}

#[test]
fn default_target_is_stable_and_git_like() {
    let target = default_target(
        std::path::Path::new("/work/acme platform"),
        "12345678-1234-4234-8234-123456789abc",
    )
    .unwrap();

    assert_eq!(target.branch, "handover/acme-platform-12345678");
    assert_eq!(
        target.worktree,
        std::path::Path::new("/work/acme platform-handover-12345678")
    );
}

#[test]
fn repository_name_sanitization_never_creates_an_invalid_component() {
    let target = default_target(
        std::path::Path::new("/work/@@@"),
        "aaaaaaaa-1234-4234-8234-123456789abc",
    )
    .unwrap();

    assert_eq!(target.branch, "handover/repo-aaaaaaaa");
    assert_eq!(
        target.worktree,
        std::path::Path::new("/work/@@@-handover-aaaaaaaa")
    );
}

#[test]
fn clean_source_reaches_the_materialization_boundary_without_mutation() {
    let fixture = ForkFixture::new();
    fixture.assert_success_at(&fixture.target);
}

#[test]
fn active_or_unrecovered_lease_is_refused_before_preflight() {
    for host in ["local-test-host", "foreign-test-host"] {
        let fixture = ForkFixture::new();
        fixture.write_lease(host);
        fixture.assert_refusal("explicit recovery is required");
    }
}

#[test]
fn sparse_checkout_is_refused_without_creating_fork_state() {
    for key in ["core.sparseCheckout", "core.sparseCheckoutCone"] {
        let fixture = ForkFixture::new();
        git(&fixture.repo, &["config", key, "true"]);
        fixture.assert_refusal("fork refuses sparse checkout");
    }
}

#[test]
fn unmerged_index_entries_are_refused_without_creating_fork_state() {
    let fixture = ForkFixture::new();
    git(&fixture.repo, &["switch", "-c", "conflict-side"]);
    std::fs::write(fixture.repo.join("README.md"), "side\n").unwrap();
    git(&fixture.repo, &["commit", "-am", "side"]);
    git(&fixture.repo, &["switch", "main"]);
    std::fs::write(fixture.repo.join("README.md"), "main\n").unwrap();
    git(&fixture.repo, &["commit", "-am", "main"]);
    let merge = Command::new("git")
        .arg("-C")
        .arg(&fixture.repo)
        .args(["merge", "conflict-side"])
        .output()
        .unwrap();
    assert!(!merge.status.success());

    fixture.assert_refusal("unmerged index entry");
}

#[test]
fn intent_to_add_is_refused_without_creating_fork_state() {
    let fixture = ForkFixture::new();
    std::fs::write(fixture.repo.join("intent.txt"), "intent\n").unwrap();
    git(&fixture.repo, &["add", "--intent-to-add", "intent.txt"]);

    fixture.assert_refusal("fork refuses intent-to-add");
}

#[test]
fn staged_gitlink_changes_are_refused_without_creating_fork_state() {
    let fixture = ForkFixture::new();
    let head = git_text(&fixture.repo, &["rev-parse", "HEAD"]);
    let cacheinfo = format!("160000,{head},linked");
    git(
        &fixture.repo,
        &["update-index", "--add", "--cacheinfo", &cacheinfo],
    );

    fixture.assert_refusal("fork refuses staged gitlink changes");
}

#[test]
fn dirty_submodules_are_refused_without_creating_fork_state() {
    let fixture = ForkFixture::new();
    let submodule = fixture.temp.path().join("submodule");
    init_repo(&submodule);
    let add = Command::new("git")
        .arg("-c")
        .arg("protocol.file.allow=always")
        .arg("-C")
        .arg(&fixture.repo)
        .args(["submodule", "add"])
        .arg(&submodule)
        .arg("vendor/sub")
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "submodule add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    git(&fixture.repo, &["commit", "-am", "add submodule"]);
    std::fs::write(fixture.repo.join("vendor/sub/README.md"), "dirty\n").unwrap();

    fixture.assert_refusal("fork refuses dirty submodules");
}

#[test]
fn unignored_special_nodes_are_refused_but_ignored_nodes_are_pruned() {
    let fixture = ForkFixture::new();
    let fifo = fixture.repo.join("agent.pipe");
    let status = Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(status.success());
    fixture.assert_refusal("fork refuses unignored special node");

    std::fs::remove_file(&fifo).unwrap();
    std::fs::write(fixture.repo.join(".gitignore"), "ignored/\n").unwrap();
    git(&fixture.repo, &["add", ".gitignore"]);
    git(&fixture.repo, &["commit", "-m", "ignore test directory"]);
    std::fs::create_dir(fixture.repo.join("ignored")).unwrap();
    let ignored_fifo = fixture.repo.join("ignored/agent.pipe");
    let status = Command::new("mkfifo").arg(&ignored_fifo).status().unwrap();
    assert!(status.success());
    fixture.assert_success_at(&fixture.target);
}

#[test]
fn non_utf8_source_or_target_metadata_is_refused() {
    let fixture = ForkFixture::new();
    let invalid_source = fixture
        .repo
        .join(OsString::from_vec(vec![b'n', b'o', b'n', b'-', 0xff]));
    match std::fs::write(&invalid_source, "opaque\n") {
        Ok(()) => fixture.assert_refusal("valid UTF-8"),
        Err(error) => assert_eq!(error.raw_os_error(), Some(92)),
    }

    let fixture = ForkFixture::new();
    let invalid_target = fixture.temp.path().join(OsString::from_vec(vec![
        b't', b'a', b'r', b'g', b'e', b't', 0xff,
    ]));
    fixture.assert_refusal_at(&invalid_target, "valid UTF-8", false);
}

#[test]
fn targets_inside_source_or_registered_worktrees_are_refused() {
    let fixture = ForkFixture::new();
    fixture.assert_refusal_at(
        &fixture.repo.join("nested-target"),
        "must not be inside the source worktree",
        false,
    );

    let fixture = ForkFixture::new();
    let other = fixture.temp.path().join("other-worktree");
    add_linked_worktree(&fixture.repo, &other, "other-worktree");
    fixture.assert_refusal_at(
        &other.join("nested-target"),
        "must not be nested inside registered worktree",
        false,
    );
}

#[test]
fn existing_target_branch_or_path_is_refused_without_replacement() {
    let fixture = ForkFixture::new();
    git(&fixture.repo, &["branch", &fixture.branch]);
    fixture.assert_refusal_at(&fixture.target, "already exists", true);

    let fixture = ForkFixture::new();
    std::fs::create_dir(&fixture.target).unwrap();
    std::fs::write(fixture.target.join("sentinel"), "keep\n").unwrap();
    fixture.assert_refusal_preserving_existing_path("already exists");
}

#[test]
fn relative_targets_resolve_from_the_callers_cwd() {
    let fixture = ForkFixture::new();
    let resolved = fixture.temp.path().join("relative-target");
    cargo_bin_cmd!("handover")
        .current_dir(&fixture.repo)
        .env("HANDOVER_HOME", &fixture.state)
        .env("PATH", &fixture.path)
        .args([
            "fork",
            "codex",
            "--branch",
            &fixture.branch,
            "--worktree",
            "../relative-target",
        ])
        .assert()
        .success();
    assert!(resolved.is_dir());
    fixture.assert_successful_fork_state(&resolved);
}

#[test]
fn a_dangling_target_symlink_is_existing_state_and_is_never_followed() {
    let fixture = ForkFixture::new();
    symlink("missing-destination", &fixture.target).unwrap();
    cargo_bin_cmd!("handover")
        .current_dir(&fixture.repo)
        .env("HANDOVER_HOME", &fixture.state)
        .arg("fork")
        .arg("codex")
        .arg("--branch")
        .arg(&fixture.branch)
        .arg("--worktree")
        .arg(&fixture.target)
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
    assert!(
        std::fs::symlink_metadata(&fixture.target)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fixture.assert_private_state_unchanged();
}

struct ForkFixture {
    temp: TempDir,
    repo: PathBuf,
    state: PathBuf,
    target: PathBuf,
    branch: String,
    source_ref_count: usize,
    path: OsString,
}

impl ForkFixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let state = temp.path().join("state");
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        write_executable(
            &bin.join("claude"),
            r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-claude 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"native","cwd":"'"$cwd_json"'","hook_event_name":"SessionStart"}' | "$HANDOVER_HOOK_BIN" __hook claude >/dev/null
exit 0
"#,
        );
        write_executable(
            &bin.join("codex"),
            r#"#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == "--version" ]]; then printf '%s\n' 'fake-codex 1.0'; exit 0; fi
cwd_json=$(printf '%s' "$PWD" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '%s' '{"session_id":"native","turn_id":"turn-1","cwd":"'"$cwd_json"'","model":"test","hook_event_name":"SessionStart"}' | "$HANDOVER_HOOK_BIN" __hook codex >/dev/null
exit 0
"#,
        );
        let path = path_with(&bin);
        cargo_bin_cmd!("handover")
            .current_dir(&repo)
            .env("HANDOVER_HOME", &state)
            .env("PATH", &path)
            .args(["run", "claude"])
            .assert()
            .success();
        let source_ref_count = std::fs::read_dir(state.join("refs/worktrees"))
            .unwrap()
            .count();
        assert_eq!(source_ref_count, 1);
        Self {
            target: temp.path().join("fork-target"),
            branch: "handover/fork-test".into(),
            temp,
            repo,
            state,
            source_ref_count,
            path,
        }
    }

    fn assert_refusal(&self, message: &str) {
        self.assert_refusal_at(&self.target, message, false);
    }

    fn assert_refusal_at(&self, target: &Path, message: &str, branch_already_exists: bool) {
        cargo_bin_cmd!("handover")
            .current_dir(&self.repo)
            .env("HANDOVER_HOME", &self.state)
            .env("PATH", &self.path)
            .arg("fork")
            .arg("codex")
            .arg("--branch")
            .arg(&self.branch)
            .arg("--worktree")
            .arg(target)
            .assert()
            .failure()
            .stderr(predicate::str::contains(message));

        assert!(!target.exists(), "fork created target {}", target.display());
        self.assert_private_state_unchanged();
        let branch_status = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["show-ref", "--verify", "--quiet"])
            .arg(format!("refs/heads/{}", self.branch))
            .status()
            .unwrap();
        assert_eq!(branch_status.success(), branch_already_exists);
    }

    fn assert_refusal_preserving_existing_path(&self, message: &str) {
        cargo_bin_cmd!("handover")
            .current_dir(&self.repo)
            .env("HANDOVER_HOME", &self.state)
            .env("PATH", &self.path)
            .args(["fork", "codex", "--branch", &self.branch, "--worktree"])
            .arg(&self.target)
            .assert()
            .failure()
            .stderr(predicate::str::contains(message));
        assert_eq!(
            std::fs::read_to_string(self.target.join("sentinel")).unwrap(),
            "keep\n"
        );
        self.assert_private_state_unchanged();
    }

    fn assert_private_state_unchanged(&self) {
        assert_eq!(
            std::fs::read_dir(self.state.join("operations"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            std::fs::read_dir(self.state.join("refs/worktrees"))
                .unwrap()
                .count(),
            self.source_ref_count
        );
    }

    fn assert_success_at(&self, target: &Path) {
        cargo_bin_cmd!("handover")
            .current_dir(&self.repo)
            .env("HANDOVER_HOME", &self.state)
            .env("PATH", &self.path)
            .arg("fork")
            .arg("codex")
            .arg("--branch")
            .arg(&self.branch)
            .arg("--worktree")
            .arg(target)
            .assert()
            .success();
        assert!(target.is_dir());
        self.assert_successful_fork_state(target);
    }

    fn assert_successful_fork_state(&self, target: &Path) {
        assert!(target.is_dir());
        assert_eq!(
            std::fs::read_dir(self.state.join("operations"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            std::fs::read_dir(self.state.join("refs/worktrees"))
                .unwrap()
                .count(),
            self.source_ref_count + 1
        );
    }

    fn write_lease(&self, host: &str) {
        let session_dir = std::fs::read_dir(self.state.join("sessions"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(session_dir.join("meta.json")).unwrap()).unwrap();
        let lease = RunLease {
            schema_version: 1,
            session_id: SessionId::parse(meta["id"].as_str().unwrap()).unwrap(),
            run_id: RunId::parse("44444444-4444-4444-8444-444444444444").unwrap(),
            provider: Provider::Claude,
            host: host.into(),
            supervisor: ProcessIdentity {
                pid: u32::MAX,
                start_token: "stale".into(),
            },
            child: None,
        };
        let path = session_dir.join("refs/active-run.json");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        let mut bytes = serde_json::to_vec_pretty(&lease).unwrap();
        bytes.push(b'\n');
        use std::io::Write;
        (&file).write_all(&bytes).unwrap();
    }
}

fn git_text(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
