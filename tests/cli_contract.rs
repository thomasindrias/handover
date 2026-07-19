use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

#[test]
fn help_identifies_the_product() {
    cargo_bin_cmd!("sesh")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Switch coding providers without losing your place",
        ));
}

#[test]
fn version_comes_from_the_package() {
    cargo_bin_cmd!("sesh")
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("sesh 0.1.0"));
}

#[test]
fn implemented_commands_are_visible_and_internal_hooks_are_hidden() {
    let output = cargo_bin_cmd!("sesh").arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    let commands: Vec<_> = help
        .split("Commands:\n")
        .nth(1)
        .unwrap()
        .lines()
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_whitespace().next())
        .filter(|command| *command != "help")
        .collect();
    assert_eq!(
        commands,
        [
            "run",
            "switch",
            "fork",
            "checkpoint",
            "status",
            "log",
            "inspect",
            "delete",
            "setup",
            "doctor",
        ]
    );
    assert!(!help.contains("__hook"));
}
