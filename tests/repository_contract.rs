use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: impl AsRef<Path>) -> String {
    let path = root().join(path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
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
        "## Why Sesh",
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
fn cargo_package_metadata_is_ready_for_a_future_release() {
    let manifest = read("Cargo.toml");
    for expected in [
        r#"license = "Apache-2.0""#,
        r#"repository = "https://github.com/thomasindrias/sesh""#,
        r#"homepage = "https://github.com/thomasindrias/sesh""#,
        r#"readme = "README.md""#,
        r#"keywords = ["ai", "cli", "developer-tools", "git", "session"]"#,
        r#"categories = ["command-line-utilities", "development-tools"]"#,
    ] {
        assert!(
            manifest.contains(expected),
            "Cargo.toml is missing {expected}"
        );
    }
}
