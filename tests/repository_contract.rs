use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: impl AsRef<Path>) -> String {
    let path = root().join(path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// Every Markdown file the repository actually ships, so a new document cannot
/// quietly escape the checks below.
fn markdown_documents() -> Vec<PathBuf> {
    fn walk(directory: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            // `target` is build output and `.superpowers` is untracked working
            // material; neither is documentation this repository publishes.
            if path.is_dir() {
                if !matches!(name.as_str(), "target" | ".git" | ".superpowers") {
                    walk(&path, found);
                }
            } else if path.extension().is_some_and(|extension| extension == "md") {
                found.push(path);
            }
        }
    }

    let mut found = Vec::new();
    walk(&root(), &mut found);
    assert!(
        found.len() >= 8,
        "the walk found suspiciously little: {found:?}"
    );
    found
}

/// The desktop transport's launch seam is an environment variable that only
/// tests set. Its `HANDOVER_TEST_` name says so, and no document may contradict
/// that by presenting it as one a user configures.
#[test]
fn test_only_environment_variables_are_not_documented_as_user_facing() {
    assert!(
        handover::launch::TEST_LAUNCH_LOG_ENV.starts_with("HANDOVER_TEST_"),
        "a variable only tests set must be named so it reads as one; found {}",
        handover::launch::TEST_LAUNCH_LOG_ENV
    );
    for document in markdown_documents() {
        assert!(
            !read(&document).contains("HANDOVER_TEST"),
            "{} documents a test-only environment variable as if a user should set it",
            document.display()
        );
    }
}

#[test]
fn repository_has_a_complete_open_source_surface() {
    let root = root();
    for required in [
        "README.md",
        "LICENSE",
        "SECURITY.md",
        "CONTRIBUTING.md",
        "CODE_OF_CONDUCT.md",
        "CHANGELOG.md",
        "docs/architecture.md",
        "docs/providers.md",
    ] {
        assert!(root.join(required).is_file(), "missing {required}");
    }
    assert!(
        !root.join("docs/superpowers").exists(),
        "internal implementation plans must not be in the public tree"
    );
    assert!(read("LICENSE").contains("Apache License\n                           Version 2.0"));
    assert!(read("SECURITY.md").contains("private vulnerability reporting"));
    assert!(read("CONTRIBUTING.md").contains("cargo test --all-targets --all-features"));
    assert!(read("CODE_OF_CONDUCT.md").contains("Contributor Covenant Code of Conduct"));
    assert!(read("CHANGELOG.md").contains("## [Unreleased]"));
}

#[test]
fn readme_is_concise_human_and_routes_details_to_focused_docs() {
    let readme = read("README.md");
    for heading in [
        "## Why Handover",
        "## Quick start",
        "## Switch or fork?",
        "## Reliability contract",
        "## Security",
        "## Project status",
        "## Contributing",
    ] {
        assert!(readme.contains(heading), "README is missing {heading}");
    }
    assert!(readme.contains("docs/architecture.md"));
    assert!(readme.contains("docs/providers.md"));
    assert!(!readme.contains(r#"{"objective":"Implement OAuth"#));
    assert!(
        readme.lines().count() <= 180,
        "README should stay concise; found {} lines",
        readme.lines().count()
    );
}

#[test]
fn security_docs_do_not_overstate_same_user_process_isolation() {
    for document in [read("docs/architecture.md"), read("docs/providers.md")] {
        assert!(!document.contains("proven to descend"));
        assert!(document.contains("does not prove process ancestry"));
        assert!(document.contains("same-user"));
    }
}

#[test]
fn cargo_package_metadata_is_ready_for_a_future_release() {
    let manifest = read("Cargo.toml");
    for expected in [
        r#"license = "Apache-2.0""#,
        r#"repository = "https://github.com/thomasindrias/handover""#,
        r#"homepage = "https://github.com/thomasindrias/handover""#,
        r#"readme = "README.md""#,
        r#"keywords = ["ai", "cli", "developer-tools", "git", "session"]"#,
        r#"categories = ["command-line-utilities", "development-tools"]"#,
    ] {
        assert!(
            manifest.contains(expected),
            "Cargo.toml is missing {expected}"
        );
    }

    assert!(read("rust-toolchain.toml").contains("channel = \"1.88.0\""));
    assert!(read("README.md").contains("Rust 1.88 or newer"));
}

#[test]
fn automation_and_community_files_are_secure_by_default() {
    for required in [
        ".github/dependabot.yml",
        ".github/PULL_REQUEST_TEMPLATE.md",
        ".github/ISSUE_TEMPLATE/bug_report.yml",
        ".github/ISSUE_TEMPLATE/config.yml",
        ".github/workflows/ci.yml",
        ".github/workflows/security.yml",
        "deny.toml",
    ] {
        assert!(root().join(required).is_file(), "missing {required}");
    }

    let ci = read(".github/workflows/ci.yml");
    assert!(ci.contains("permissions:\n  contents: read"));
    assert!(ci.contains("ubuntu-latest"));
    assert!(ci.contains("macos-latest"));
    assert!(ci.contains("cargo fmt --check"));
    assert!(ci.contains("cargo clippy --all-targets --all-features -- -D warnings"));
    assert!(ci.contains("cargo test --all-targets --all-features"));

    let security = read(".github/workflows/security.yml");
    assert!(security.contains("permissions:\n  contents: read"));
    assert!(security.contains("cargo install cargo-audit --version 0.22.2 --locked"));
    assert!(security.contains("run: cargo audit"));
    assert!(security.contains("EmbarkStudios/cargo-deny-action@"));

    for workflow in [ci, security] {
        for line in workflow.lines() {
            let Some((_, reference)) = line
                .trim()
                .strip_prefix("- uses: ")
                .and_then(|line| line.split_once('@'))
            else {
                continue;
            };
            let revision = reference.split_whitespace().next().unwrap();
            assert_eq!(
                revision.len(),
                40,
                "action is not pinned to a full SHA: {line}"
            );
            assert!(
                revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "action is not pinned to a commit SHA: {line}"
            );
        }
    }

    let dependabot = read(".github/dependabot.yml");
    assert!(dependabot.contains("package-ecosystem: cargo"));
    assert!(dependabot.contains("package-ecosystem: github-actions"));
    assert_eq!(dependabot.matches("interval: weekly").count(), 2);

    let issue_config = read(".github/ISSUE_TEMPLATE/config.yml");
    assert!(issue_config.contains("blank_issues_enabled: false"));
    assert!(issue_config.contains("SECURITY.md"));
}
