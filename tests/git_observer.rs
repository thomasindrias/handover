mod support;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};

use handover::git::Git;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[test]
fn observes_linked_worktree_nested_cwd_and_all_dirty_classes() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("oauth worktree");
    support::init_repo(&repo);
    support::git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feat/oauth",
            worktree.to_str().unwrap(),
        ],
    );
    let cwd = worktree.join("apps/web");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(worktree.join("README.md"), "staged\n").unwrap();
    support::git(&worktree, &["add", "README.md"]);
    std::fs::write(worktree.join("README.md"), "unstaged\n").unwrap();
    std::fs::write(cwd.join("new file.txt"), "untracked\n").unwrap();
    symlink("new file.txt", cwd.join("new-link")).unwrap();

    let snapshot = Git::new().snapshot(&cwd).unwrap();

    assert_eq!(snapshot.identity.worktree, worktree.canonicalize().unwrap());
    assert_eq!(
        snapshot.identity.cwd_relative,
        std::path::Path::new("apps/web")
    );
    assert_eq!(snapshot.branch.as_deref(), Some("feat/oauth"));
    assert!(
        snapshot
            .staged
            .iter()
            .any(|path| path.path == std::path::Path::new("README.md"))
    );
    assert!(
        snapshot
            .unstaged
            .iter()
            .any(|path| path.path == std::path::Path::new("README.md"))
    );
    assert_eq!(
        snapshot
            .staged
            .iter()
            .find(|path| path.path == std::path::Path::new("README.md"))
            .unwrap()
            .sha256,
        Some(hex::encode(Sha256::digest(b"staged\n")))
    );
    assert_eq!(
        snapshot
            .unstaged
            .iter()
            .find(|path| path.path == std::path::Path::new("README.md"))
            .unwrap()
            .sha256,
        Some(hex::encode(Sha256::digest(b"unstaged\n")))
    );
    assert!(
        snapshot
            .untracked
            .iter()
            .any(|path| path.path == std::path::Path::new("apps/web/new file.txt"))
    );
    assert!(snapshot.untracked.iter().any(|path| {
        path.symlink_target.as_deref() == Some(std::path::Path::new("new file.txt"))
    }));
}

#[test]
fn reports_only_actually_dirty_submodules() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    let dependency = temp.path().join("dependency");
    support::init_repo(&repo);
    support::init_repo(&dependency);
    support::git(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            dependency.to_str().unwrap(),
            "vendor/dependency",
        ],
    );
    support::git(&repo, &["commit", "-m", "add dependency"]);

    assert!(
        Git::new()
            .snapshot(&repo)
            .unwrap()
            .dirty_submodules
            .is_empty()
    );

    std::fs::write(repo.join("vendor/dependency/README.md"), "dirty\n").unwrap();
    let snapshot = Git::new().snapshot(&repo).unwrap();

    assert_eq!(
        snapshot.dirty_submodules,
        [std::path::PathBuf::from("vendor/dependency")]
    );
}

#[test]
fn detached_head_is_a_supported_absent_branch() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    support::init_repo(&repo);
    support::git(&repo, &["checkout", "--detach"]);

    let snapshot = Git::new().snapshot(&repo).unwrap();

    assert_eq!(snapshot.branch, None);
}

#[test]
fn observation_is_read_only_and_treats_git_magic_names_literally() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    support::init_repo(&repo);
    let magic = ":(glob)*.txt";
    std::fs::write(repo.join(magic), b"staged\n").unwrap();
    support::git(&repo, &["add", "--", magic]);
    std::fs::write(repo.join(magic), b"unstaged\n").unwrap();
    let index_before = std::fs::read(repo.join(".git/index")).unwrap();

    let snapshot = Git::new().snapshot(&repo).unwrap();

    assert_eq!(
        std::fs::read(repo.join(".git/index")).unwrap(),
        index_before
    );
    assert!(
        snapshot
            .staged
            .iter()
            .any(|path| path.path == std::path::Path::new(magic))
    );
    assert!(
        snapshot
            .unstaged
            .iter()
            .any(|path| path.path == std::path::Path::new(magic))
    );
}

#[test]
fn index_and_worktree_facts_preserve_deletion_modes_and_symlink_targets() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    support::init_repo(&repo);
    std::fs::write(repo.join("README.md"), b"staged executable\n").unwrap();
    std::fs::set_permissions(
        repo.join("README.md"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    symlink("staged-target", repo.join("current-link")).unwrap();
    support::git(&repo, &["add", "README.md", "current-link"]);
    std::fs::remove_file(repo.join("README.md")).unwrap();
    std::fs::remove_file(repo.join("current-link")).unwrap();
    symlink("unstaged-target", repo.join("current-link")).unwrap();

    let snapshot = Git::new().snapshot(&repo).unwrap();
    let staged_file = snapshot
        .staged
        .iter()
        .find(|path| path.path == std::path::Path::new("README.md"))
        .unwrap();
    let unstaged_file = snapshot
        .unstaged
        .iter()
        .find(|path| path.path == std::path::Path::new("README.md"))
        .unwrap();
    let staged_link = snapshot
        .staged
        .iter()
        .find(|path| path.path == std::path::Path::new("current-link"))
        .unwrap();
    let unstaged_link = snapshot
        .unstaged
        .iter()
        .find(|path| path.path == std::path::Path::new("current-link"))
        .unwrap();

    assert!(staged_file.executable);
    assert_eq!(
        staged_file.sha256,
        Some(hex::encode(Sha256::digest(b"staged executable\n")))
    );
    assert_eq!(unstaged_file.sha256, None);
    assert_eq!(
        staged_link.symlink_target.as_deref(),
        Some(std::path::Path::new("staged-target"))
    );
    assert_eq!(
        unstaged_link.symlink_target.as_deref(),
        Some(std::path::Path::new("unstaged-target"))
    );
}

#[test]
fn refuses_non_utf8_untracked_paths_and_symlink_targets() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    support::init_repo(&repo);
    let non_utf8 = repo.join(OsString::from_vec(b"invalid-\xff".to_vec()));
    match std::fs::write(&non_utf8, b"unsupported") {
        Ok(()) => {
            assert!(Git::new().snapshot(&repo).is_err());
            std::fs::remove_file(non_utf8).unwrap();
        }
        Err(error) if cfg!(target_os = "macos") && error.raw_os_error() == Some(libc::EILSEQ) => {}
        Err(error) => panic!("cannot create non-UTF-8 path fixture: {error}"),
    }

    let invalid_target = std::path::PathBuf::from(OsString::from_vec(b"target-\xff".to_vec()));
    match symlink(&invalid_target, repo.join("invalid-target-link")) {
        Ok(()) => assert!(Git::new().snapshot(&repo).is_err()),
        Err(error) if cfg!(target_os = "macos") && error.raw_os_error() == Some(libc::EILSEQ) => {}
        Err(error) => panic!("cannot create non-UTF-8 symlink target fixture: {error}"),
    }
}
