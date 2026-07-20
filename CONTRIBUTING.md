# Contributing to Sesh

Thank you for helping make provider switching boringly reliable. Focused bug
reports, tests, documentation improvements, and small changes are welcome.

## Before you start

- Search existing issues and pull requests.
- Open an issue before a large change or a change to the session format.
- Report security problems privately as described in [SECURITY.md](SECURITY.md).
- Keep V1 focused on local session continuity for macOS and Linux.

## Development

Install stable Rust with `rustfmt` and `clippy`, then clone the repository:

```bash
git clone https://github.com/thomasindrias/sesh.git
cd sesh
cargo build
```

Sesh follows test-driven development. Add or adjust a focused test, observe the
failure, implement the smallest complete change, and refactor only while the
suite stays green. Tests must not call a real model or require provider login.

Run the complete local gate before opening a pull request:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo doc --no-deps --document-private-items
```

Provider smoke tests are intentionally ignored by default. Their commands and
safety boundaries are documented in [docs/providers.md](docs/providers.md).

## Pull requests

Keep commits intentional and the diff narrowly scoped. In the pull request:

- explain the user-visible problem and the chosen behavior;
- link the issue or design discussion when one exists;
- describe the tests that prove the change;
- call out storage compatibility, Git mutations, or security implications;
- update the README, focused docs, and changelog when behavior changes.

By contributing, you agree that your contribution is licensed under the
[Apache License 2.0](LICENSE). All participation is governed by the
[Code of Conduct](CODE_OF_CONDUCT.md).
