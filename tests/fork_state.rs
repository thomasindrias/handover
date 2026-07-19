mod support;

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use sesh::fork::{ForkOperationStore, capture_fork_artifacts};
use sesh::git::Git;
use sesh::git::fork::materialize;
use sesh::model::{ForkOperation, ForkPhase, OperationId, SessionId};
use sesh::store::StateLayout;
use sha2::{Digest, Sha256};
use support::{add_linked_worktree, git, init_repo, repository_fingerprint};
use tempfile::TempDir;

#[test]
fn materialize_duplicates_the_exact_index_worktree_and_untracked_state() {
    let fixture = StateFixture::new();
    let before = repository_fingerprint(&fixture.source);
    let before_index = git_bytes(&fixture.source, &["ls-files", "--stage", "-z"]);
    let before_branch = git_text(&fixture.source, &["branch", "--show-current"]);
    let before_head = git_text(&fixture.source, &["rev-parse", "HEAD"]);

    let store = fixture.capture();
    let staged_hash = sha256_file(&store.operation_dir().join("staged.patch"));
    let unstaged_hash = sha256_file(&store.operation_dir().join("unstaged.patch"));
    let operation = materialize(&store, &fixture.source_cwd, |_| Ok(())).unwrap();

    assert_eq!(operation.phase, ForkPhase::Verified);
    let source = Git::new().snapshot(&fixture.source_cwd).unwrap();
    let target_cwd = fixture.target.join("apps/web");
    let target = Git::new().snapshot(&target_cwd).unwrap();
    assert_eq!(target.head, source.head);
    assert_eq!(target.staged, source.staged);
    assert_eq!(target.unstaged, source.unstaged);
    assert_eq!(target.untracked, source.untracked);
    assert!(target.dirty_submodules.is_empty());
    assert_eq!(
        target.identity.common_git_dir,
        source.identity.common_git_dir
    );
    assert_eq!(target.identity.cwd_relative, source.identity.cwd_relative);
    assert_eq!(target.branch.as_deref(), Some(fixture.branch.as_str()));
    assert_eq!(
        std::fs::read(fixture.target.join("same.txt")).unwrap(),
        b"unstaged version\n"
    );
    assert!(!fixture.target.join("ignored.secret").exists());
    assert_eq!(
        std::fs::metadata(fixture.target.join("mode.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        std::fs::read_link(fixture.target.join("tracked-link")).unwrap(),
        Path::new("link-target-b")
    );

    assert_eq!(repository_fingerprint(&fixture.source), before);
    assert_eq!(
        git_bytes(&fixture.source, &["ls-files", "--stage", "-z"]),
        before_index
    );
    assert_eq!(
        git_text(&fixture.source, &["branch", "--show-current"]),
        before_branch
    );
    assert_eq!(
        git_text(&fixture.source, &["rev-parse", "HEAD"]),
        before_head
    );
    assert_eq!(
        sha256_file(&store.operation_dir().join("staged.patch")),
        staged_hash
    );
    assert_eq!(
        sha256_file(&store.operation_dir().join("unstaged.patch")),
        unstaged_hash
    );
}

#[test]
fn verification_refuses_a_source_mutation_after_target_creation() {
    let fixture = StateFixture::new();
    let store = fixture.capture();
    let error = materialize(&store, &fixture.source_cwd, |phase| {
        if phase == ForkPhase::WorktreeCreated {
            std::fs::write(fixture.source.join("same.txt"), "source moved on\n").unwrap();
        }
        Ok(())
    })
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("source changed during fork materialization")
    );
    assert!(fixture.target.exists());
    assert_ne!(
        Git::new().fingerprint(&fixture.source_cwd).unwrap(),
        ForkOperationStore::read(&fixture.layout, store.id().clone())
            .unwrap()
            .source_fingerprint
            .unwrap()
    );
}

#[test]
fn materialize_refuses_an_ignored_target_path_outside_the_allowed_inventory() {
    let fixture = StateFixture::new();
    let store = fixture.capture();
    let error = materialize(&store, &fixture.source_cwd, |phase| {
        if phase == ForkPhase::WorktreeCreated {
            std::fs::write(fixture.target.join("ignored.secret"), "intruder\n").unwrap();
        }
        Ok(())
    })
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("target changed after its last durable fork phase")
    );
    assert_eq!(store.operation().unwrap().phase, ForkPhase::WorktreeCreated);
    assert_eq!(
        std::fs::read_to_string(fixture.target.join("ignored.secret")).unwrap(),
        "intruder\n"
    );
}

#[test]
fn materialize_restores_only_recorded_initialized_submodules_without_a_protocol() {
    let temp = TempDir::new().unwrap();
    let leaf = temp.path().join("leaf");
    init_repo(&leaf);
    let middle = temp.path().join("middle");
    init_repo(&middle);
    git_file_protocol(
        &middle,
        &["submodule", "add", leaf.to_str().unwrap(), "nested/leaf"],
    );
    git(&middle, &["commit", "-am", "add nested leaf"]);

    let repository = temp.path().join("repository");
    init_repo(&repository);
    for path in ["vendor/initialized", "vendor/mixed", "vendor/uninitialized"] {
        git_file_protocol(
            &repository,
            &["submodule", "add", middle.to_str().unwrap(), path],
        );
    }
    git(&repository, &["commit", "-am", "add submodule topology"]);
    let source = temp.path().join("source-worktree");
    add_linked_worktree(&repository, &source, "submodule-source");
    git_file_protocol(
        &source,
        &[
            "submodule",
            "update",
            "--init",
            "--",
            "vendor/initialized",
            "vendor/mixed",
        ],
    );
    git_file_protocol(
        &source,
        &[
            "submodule",
            "update",
            "--init",
            "--recursive",
            "--",
            "vendor/initialized",
        ],
    );
    let before = repository_fingerprint(&source);

    let target = temp.path().join("target-worktree");
    let layout = StateLayout::new(temp.path().join("state"));
    let source_snapshot = Git::new().snapshot(&source).unwrap();
    let operation = ForkOperation {
        schema_version: 1,
        id: OperationId::parse("88888888-8888-4888-8888-888888888888").unwrap(),
        phase: ForkPhase::Prepared,
        source_session_id: SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
        source_worktree: source_snapshot.identity,
        source_checkpoint_sequence: None,
        source_fingerprint: None,
        target_branch: "sesh/submodule-copy".into(),
        target_worktree: target.clone(),
        target_head: source_snapshot.head,
        child_session_id: None,
        target_fingerprint: None,
        target_cleanup_inventory_sha256: None,
        branch_created: false,
        target_created: false,
        error: None,
        updated_at: "2026-07-19T10:00:00Z".into(),
    };
    let store = ForkOperationStore::create(&layout, &operation).unwrap();
    capture_fork_artifacts(&store, &source, || Ok(())).unwrap();
    materialize(&store, &source, |_| Ok(())).unwrap();

    assert!(target.join("vendor/initialized/.git").exists());
    assert!(target.join("vendor/initialized/nested/leaf/.git").exists());
    assert!(target.join("vendor/mixed/.git").exists());
    assert!(!target.join("vendor/mixed/nested/leaf/.git").exists());
    assert!(!target.join("vendor/uninitialized/.git").exists());
    assert_eq!(
        git_text(
            &target.join("vendor/initialized/nested/leaf"),
            &["rev-parse", "HEAD"]
        ),
        git_text(&leaf, &["rev-parse", "HEAD"])
    );
    assert_eq!(repository_fingerprint(&source), before);
}

struct StateFixture {
    _temp: TempDir,
    source: PathBuf,
    source_cwd: PathBuf,
    target: PathBuf,
    layout: StateLayout,
    branch: String,
}

impl StateFixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let repository = temp.path().join("repository");
        init_repo(&repository);
        for (path, bytes) in [
            ("index-only.txt", b"base index\n".as_slice()),
            ("worktree-only.txt", b"base worktree\n".as_slice()),
            ("same.txt", b"base same\n".as_slice()),
            ("staged-delete.txt", b"delete staged\n".as_slice()),
            ("unstaged-delete.txt", b"delete unstaged\n".as_slice()),
            ("rename-old.txt", b"rename\n".as_slice()),
            ("binary-staged.bin", &[0, 1, 2, 0xff]),
            ("binary-unstaged.bin", &[0, 3, 4, 0xfe]),
            ("mode.txt", b"#!/bin/sh\nexit 0\n".as_slice()),
            ("link-target-a", b"a\n".as_slice()),
            ("link-target-b", b"b\n".as_slice()),
        ] {
            std::fs::write(repository.join(path), bytes).unwrap();
        }
        symlink("link-target-a", repository.join("tracked-link")).unwrap();
        std::fs::write(repository.join(".gitignore"), "ignored.secret\n").unwrap();
        std::fs::create_dir_all(repository.join("apps/web")).unwrap();
        std::fs::write(repository.join("apps/web/tracked.txt"), "cwd anchor\n").unwrap();
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "-m", "state matrix base"]);

        let source = temp.path().join("source-worktree");
        add_linked_worktree(&repository, &source, "fork-source");
        let source_cwd = source.join("apps/web");
        std::fs::write(source.join("index-only.txt"), "index version\n").unwrap();
        git(&source, &["add", "index-only.txt"]);
        std::fs::write(source.join("worktree-only.txt"), "worktree version\n").unwrap();
        std::fs::write(source.join("same.txt"), "staged version\n").unwrap();
        git(&source, &["add", "same.txt"]);
        std::fs::write(source.join("same.txt"), "unstaged version\n").unwrap();
        git(&source, &["rm", "staged-delete.txt"]);
        std::fs::remove_file(source.join("unstaged-delete.txt")).unwrap();
        git(&source, &["mv", "rename-old.txt", "rename-new.txt"]);
        std::fs::write(source.join("binary-staged.bin"), [0, 9, 8, 0xff, 0]).unwrap();
        git(&source, &["add", "binary-staged.bin"]);
        std::fs::write(source.join("binary-unstaged.bin"), [0, 7, 6, 0xfe, 0]).unwrap();
        std::fs::set_permissions(
            source.join("mode.txt"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::remove_file(source.join("tracked-link")).unwrap();
        symlink("link-target-b", source.join("tracked-link")).unwrap();
        std::fs::create_dir_all(source.join("nested dir")).unwrap();
        std::fs::write(source.join("nested dir/file with spaces.bin"), [0, 5, 0xff]).unwrap();
        std::fs::write(source.join("untracked-exec.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            source.join("untracked-exec.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        symlink(
            "nested dir/file with spaces.bin",
            source.join("untracked-link"),
        )
        .unwrap();
        std::fs::write(source.join("ignored.secret"), "do not copy\n").unwrap();

        let state = temp.path().join("state");
        let target = temp.path().join("target-worktree");
        let layout = StateLayout::new(state.clone());
        Self {
            _temp: temp,
            source,
            source_cwd,
            target,
            layout,
            branch: "sesh/state-copy".into(),
        }
    }

    fn capture(&self) -> ForkOperationStore {
        let source = Git::new().snapshot(&self.source_cwd).unwrap();
        let operation = ForkOperation {
            schema_version: 1,
            id: OperationId::parse("77777777-7777-4777-8777-777777777777").unwrap(),
            phase: ForkPhase::Prepared,
            source_session_id: SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
            source_worktree: source.identity,
            source_checkpoint_sequence: None,
            source_fingerprint: None,
            target_branch: self.branch.clone(),
            target_worktree: self.target.clone(),
            target_head: source.head,
            child_session_id: None,
            target_fingerprint: None,
            target_cleanup_inventory_sha256: None,
            branch_created: false,
            target_created: false,
            error: None,
            updated_at: "2026-07-19T10:00:00Z".into(),
        };
        let store = ForkOperationStore::create(&self.layout, &operation).unwrap();
        capture_fork_artifacts(&store, &self.source_cwd, || Ok(())).unwrap();
        store
    }
}

fn git_bytes(cwd: &Path, args: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new("git")
        .arg("--no-pager")
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
    output.stdout
}

fn git_text(cwd: &Path, args: &[&str]) -> String {
    String::from_utf8(git_bytes(cwd, args))
        .unwrap()
        .trim()
        .into()
}

fn sha256_file(path: &Path) -> String {
    hex::encode(Sha256::digest(std::fs::read(path).unwrap()))
}

fn git_file_protocol(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-c")
        .arg("protocol.file.allow=always")
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
}
