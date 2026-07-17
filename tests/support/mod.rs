use std::path::Path;
use std::process::Command;

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
