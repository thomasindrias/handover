# Secure Open-Source Publication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish Sesh as a hardened private GitHub repository with a concise open-source-ready tree and a reviewable draft pull request into protected `main`.

**Architecture:** Treat repository presentation, supply-chain policy, and GitHub publication as three gated layers. Local contract tests and security scans must pass before GitHub is created; repository settings are applied while it is private; verification reads every important remote setting back before the task is complete.

**Tech Stack:** Rust/Cargo, Git, GitHub Actions, GitHub CLI/API, Apache-2.0, Cargo Audit, Cargo Deny, Gitleaks.

---

### Task 1: Freeze the visible repository contract

**Files:**
- Create: `tests/repository_contract.rs`
- Modify: `Cargo.toml`
- Modify: `README.md`
- Create: `LICENSE`
- Create: `SECURITY.md`
- Create: `CONTRIBUTING.md`
- Create: `CODE_OF_CONDUCT.md`
- Create: `CHANGELOG.md`
- Create: `docs/architecture.md`
- Create: `docs/providers.md`
- Delete: `docs/superpowers/`

- [ ] **Step 1: Write the failing repository contract test**

Create `tests/repository_contract.rs` with tests that resolve
`env!("CARGO_MANIFEST_DIR")` and assert:

```rust
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
assert!(!root.join("docs/superpowers").exists());
```

Also assert that README contains `Why Sesh`, `Quick start`, `Switch or fork?`,
`Reliability contract`, `Security`, `Project status`, and `Contributing`; that it
links to both focused docs; that it does not contain the long checkpoint JSON;
and that Cargo metadata contains:

```toml
license = "Apache-2.0"
repository = "https://github.com/thomasindrias/sesh"
homepage = "https://github.com/thomasindrias/sesh"
readme = "README.md"
keywords = ["ai", "cli", "developer-tools", "git", "session"]
categories = ["command-line-utilities", "development-tools"]
```

- [ ] **Step 2: Run the contract test and prove it fails**

Run:

```bash
rtk cargo test --test repository_contract -- --nocapture
```

Expected: FAIL because the community files and focused documentation do not yet
exist and `docs/superpowers` is still visible.

- [ ] **Step 3: Implement the concise repository surface**

Rewrite README to this information order:

```text
# Sesh
one-sentence outcome
Why Sesh
Quick start (setup, run, switch, fork)
Switch or fork? (two-row comparison)
Reliability contract (facts preserved and explicit exclusions)
Security (plaintext local state and same-user boundary)
Project status (V1, macOS/Linux, Claude/Codex)
Contributing
License
```

Move the existing transaction, storage, recovery, UTF-8, deletion, and provider
protocol details into `docs/architecture.md`. Move provider setup, hook trust,
checkpoint input, and optional provider smoke-test commands into
`docs/providers.md`. Commands copied from the existing README and design spec
must remain byte-for-byte executable.

Add the canonical Apache License 2.0 text to `LICENSE`. Add a security policy
that directs reports to GitHub private vulnerability reporting and explicitly
states that session/operation data is plaintext and unrestricted same-user
provider shells are outside Sesh's isolation guarantees. Add contribution steps
using `cargo fmt`, `cargo clippy`, `cargo test --all-targets --all-features`, and
TDD. Add Contributor Covenant 2.1 with `thomasindrias` as the enforcement contact
through GitHub. Add an Unreleased V1 changelog entry without inventing a release
date.

Update Cargo metadata exactly as asserted. Remove the complete
`docs/superpowers` directory from the visible tree after copying only the proven
architecture and provider material.

- [ ] **Step 4: Verify repository presentation and packaging**

Run:

```bash
rtk cargo test --test repository_contract -- --nocapture
rtk cargo metadata --no-deps --format-version 1
rtk cargo package --allow-dirty
rtk rg -n "TODO|TBD|your-email|example.com" README.md SECURITY.md CONTRIBUTING.md CODE_OF_CONDUCT.md CHANGELOG.md docs Cargo.toml
```

Expected: contract and package PASS; placeholder scan returns no matches except
the Apache license's normative example-domain text, if present.

- [ ] **Step 5: Commit the repository surface**

```bash
rtk git add README.md Cargo.toml LICENSE SECURITY.md CONTRIBUTING.md CODE_OF_CONDUCT.md CHANGELOG.md docs tests/repository_contract.rs
rtk git commit -m "docs: prepare Sesh for open source"
```

### Task 2: Add deterministic repository security policy

**Files:**
- Modify: `.github/workflows/ci.yml`
- Create: `.github/workflows/security.yml`
- Create: `.github/dependabot.yml`
- Create: `.github/PULL_REQUEST_TEMPLATE.md`
- Create: `.github/ISSUE_TEMPLATE/bug.yml`
- Create: `.github/ISSUE_TEMPLATE/feature.yml`
- Create: `.github/ISSUE_TEMPLATE/config.yml`
- Create: `deny.toml`
- Modify: `tests/repository_contract.rs`

- [ ] **Step 1: Extend the failing contract to workflows and community files**

Assert all listed files exist. Parse workflow text and assert:

```text
top-level `permissions:` exists
`contents: read` exists
no `write-all` exists
every `uses:` value ends in `@` followed by exactly 40 lowercase hex characters
provider credentials and provider-authenticated jobs are absent
```

Assert Dependabot contains weekly `cargo` and `github-actions` ecosystems with
open-pull-request limits. Assert the issue configuration disables blank issues
and links security reports to `SECURITY.md`.

- [ ] **Step 2: Run the contract and prove unpinned actions fail**

Run `rtk cargo test --test repository_contract -- --nocapture`.

Expected: FAIL on existing floating `actions/checkout@v4` and
`dtolnay/rust-toolchain@stable` references and missing security/community files.

- [ ] **Step 3: Resolve and pin official actions**

Use official GitHub repositories and release tags to resolve immutable commits
for `actions/checkout`, `dtolnay/rust-toolchain`, and the selected official
RustSec audit action. Record the release tag in a trailing YAML comment while
using the full commit SHA in `uses:`. Do not use marketplace wrappers when an
official upstream action exists.

Set workflow defaults to:

```yaml
permissions:
  contents: read
```

Keep the Linux/macOS CI matrix and add concurrency cancellation scoped by
workflow and ref. Add a separate security workflow for pull requests, pushes to
`main`, and a weekly schedule. It runs Cargo Audit and `cargo deny check` without
write permissions.

- [ ] **Step 4: Add dependency and community policy**

Create `deny.toml` with explicit accepted SPDX licenses for the resolved
dependency graph, deny unknown registries and Git sources, deny advisories and
yanked crates, and warn on duplicate versions until the graph can remove them
without unrelated upgrades. Add weekly Dependabot configuration and concise bug,
feature, PR, and security-routing templates.

- [ ] **Step 5: Verify security policy locally**

Install missing tools from crates.io with locked resolution:

```bash
rtk cargo install cargo-audit --locked
rtk cargo install cargo-deny --locked
```

Then run:

```bash
rtk cargo audit
rtk cargo deny check
rtk cargo test --test repository_contract -- --nocapture
rtk cargo fmt --check
rtk cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all PASS. Advisory or license exceptions are allowed only when the
checked-in policy names the exact advisory/crate and explains why it is safe.

- [ ] **Step 6: Commit security policy**

```bash
rtk git add .github deny.toml tests/repository_contract.rs
rtk git commit -m "ci: harden repository security policy"
```

### Task 3: Prove the complete tree and history are publishable

**Files:**
- Modify only if a scan finds a real issue; any remediation must be isolated in
  its own commit and the complete scan rerun.

- [ ] **Step 1: Install and verify Gitleaks from its official distribution**

Install Gitleaks using the system package manager or its verified official
release. Run `rtk gitleaks version` and retain the reported version in the final
evidence.

- [ ] **Step 2: Scan reachable history and the current tree**

Run:

```bash
rtk gitleaks git --redact --no-banner --exit-code 1 .
rtk gitleaks dir --redact --no-banner --exit-code 1 .
```

Expected: both exit zero with no findings. Do not push if either reports a
finding. A false positive must be documented in a narrowly scoped
`.gitleaks.toml` rule with the exact fingerprint; broad path exclusions are not
allowed.

- [ ] **Step 3: Run the complete release gate**

```bash
rtk cargo fmt --check
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo test --all-targets --all-features
rtk cargo doc --no-deps
rtk cargo audit
rtk cargo deny check
rtk cargo package --allow-dirty
rtk git status --short
```

Expected: all checks PASS and the worktree is clean.

### Task 4: Create and secure the private GitHub repository

**Files:**
- No repository file changes.

- [ ] **Step 1: Reconfirm authentication and name availability**

```bash
rtk gh auth status
rtk gh repo view thomasindrias/sesh --json nameWithOwner,visibility,url
```

Expected: authenticated as `thomasindrias`; repository lookup returns not found.
If it exists, stop unless it is private, empty, and clearly belongs to this
project.

- [ ] **Step 2: Create the private repository without generated files**

```bash
rtk gh repo create thomasindrias/sesh --private --description "Switch AI coding providers without losing your place." --disable-wiki
rtk git remote add origin https://github.com/thomasindrias/sesh.git
```

Verify `rtk gh repo view thomasindrias/sesh --json visibility,isPrivate,url` and
require `PRIVATE`/`true` before any push.

- [ ] **Step 3: Push `main`, set defaults, and push the feature branch**

```bash
rtk git push -u origin main
rtk gh repo edit thomasindrias/sesh --default-branch main --enable-issues --delete-branch-on-merge --enable-merge-commit=false --enable-rebase-merge=true --enable-squash-merge=true
rtk git push -u origin feat/v1-foundation
```

Do not force push and do not rewrite either branch.

- [ ] **Step 4: Apply repository security settings**

Use `gh api` to enable vulnerability alerts and automated security fixes. Enable
private vulnerability reporting and secret scanning/push protection when the
account supports them. Create an active `main` ruleset that:

```text
targets refs/heads/main
requires a pull request with 0 approvals
requires conversation resolution
requires required status checks after their exact check names are observed
requires linear history
blocks force pushes and deletion
```

If private-repository rulesets or secret scanning are unavailable, keep the
repository private, retain workflow checks, and record the exact API response.

- [ ] **Step 5: Commit no local changes**

Run `rtk git status --short` and require empty output.

### Task 5: Open and verify the first draft pull request

**Files:**
- No repository file changes.

- [ ] **Step 1: Open the draft PR**

Create a draft PR titled `Build Sesh V1` from `feat/v1-foundation` into `main`.
The body must contain:

```text
Summary: provider-neutral local sessions, exact worktree continuation, explicit fork
Why: remove provider lock-in and context rebuilding
Security: plaintext state boundary, private initial visibility, supply-chain gates
Validation: exact fmt/clippy/test/doc/audit/deny/gitleaks/package commands and results
Non-goals: no merge, release, crates.io publish, cloud sync, or public visibility
```

- [ ] **Step 2: Read back and verify remote state**

Use `gh repo view`, `gh pr view`, `gh api repos/thomasindrias/sesh`, branch and
ruleset endpoints, vulnerability-alert endpoints, and `git ls-remote origin`.
Verify:

```text
repository is PRIVATE
default branch is main
main and feat/v1-foundation point to the expected local commits
draft PR targets main and has no auto-merge
README, LICENSE, SECURITY, CONTRIBUTING, and workflows are visible on the PR branch
ruleset is active or its account limitation is recorded
vulnerability/security settings are enabled or their limitation is recorded
```

- [ ] **Step 3: Confirm final local state**

Run:

```bash
rtk git status --short --branch
rtk git remote -v
rtk git log -5 --oneline
```

Expected: clean `feat/v1-foundation` tracking
`origin/feat/v1-foundation`, with only the canonical GitHub remote.
