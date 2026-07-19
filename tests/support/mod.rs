use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

#[derive(Debug, Eq, PartialEq)]
pub struct RepositoryFingerprint {
    worktree: String,
    index: String,
}

pub fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("--no-pager")
        .arg("--literal-pathspecs")
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

pub fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.name", "Sesh Test"]);
    git(path, &["config", "user.email", "sesh@example.invalid"]);
    std::fs::write(path.join("README.md"), "initial\n").unwrap();
    git(path, &["add", "README.md"]);
    git(path, &["commit", "-m", "initial"]);
}

#[allow(dead_code)]
pub fn add_linked_worktree(repository: &Path, worktree: &Path, branch: &str) {
    let output = Command::new("git")
        .arg("--no-pager")
        .arg("-C")
        .arg(repository)
        .args(["worktree", "add", "-b", branch])
        .arg(worktree)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[allow(dead_code)]
pub fn path_with(bin: &Path) -> OsString {
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).unwrap()
}

#[allow(dead_code)]
pub fn repository_fingerprint(worktree: &Path) -> RepositoryFingerprint {
    let mut entries = Vec::new();
    collect_worktree_entries(worktree, worktree, &mut entries);
    entries.sort();
    let mut worktree_hash = Sha256::new();
    for path in entries {
        let relative = path.strip_prefix(worktree).unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        worktree_hash.update(relative.as_os_str().as_bytes());
        worktree_hash.update([0]);
        worktree_hash.update((metadata.permissions().mode() & 0o777).to_be_bytes());
        if metadata.file_type().is_symlink() {
            worktree_hash.update(b"symlink\0");
            worktree_hash.update(std::fs::read_link(path).unwrap().as_os_str().as_bytes());
        } else if metadata.is_file() {
            worktree_hash.update(b"file\0");
            worktree_hash.update(std::fs::read(path).unwrap());
        } else if metadata.is_dir() {
            worktree_hash.update(b"directory\0");
        }
        worktree_hash.update([0xff]);
    }

    let output = Command::new("git")
        .arg("--no-pager")
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "--path-format=absolute", "--git-path", "index"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git rev-parse index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let index = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());

    RepositoryFingerprint {
        worktree: hex::encode(worktree_hash.finalize()),
        index: hex::encode(Sha256::digest(std::fs::read(index).unwrap())),
    }
}

fn collect_worktree_entries(root: &Path, directory: &Path, entries: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        entries.push(path.clone());
        if std::fs::symlink_metadata(&path).unwrap().is_dir() {
            collect_worktree_entries(root, &path, entries);
        }
    }
    assert!(directory.starts_with(root));
}

#[allow(dead_code)]
pub fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}
