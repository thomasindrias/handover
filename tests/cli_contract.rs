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
