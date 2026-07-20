# Secure Open-Source Publication Design

## Goal

Publish Sesh to a new private GitHub repository at `thomasindrias/sesh` through
a reviewable pull request, with a concise human-facing repository, an explicit
Apache-2.0 license, deterministic security checks, and conservative GitHub
settings suitable for making the project public after merge.

## Starting Point

The local repository has two branches and no remote:

- `main` contains the original V1 architecture commit.
- `feat/v1-foundation` contains the complete TDD implementation history and is
  clean with the V1 release gate passing.

The existing history will be preserved. Before any push, the complete reachable
history and current worktree must pass secret scanning. Internal implementation
plans will be removed from the visible feature-branch tree, but their historical
presence is acceptable only after the history scan proves that they contain no
credentials or other secrets.

## Repository Content

The root README will be rewritten for a developer encountering Sesh for the
first time. It will lead with the problem and value, provide a short quick start,
explain `switch` versus `fork`, state the reliability and security boundaries,
and link to detailed documentation. Long JSON examples, storage schemas,
operation phases, and recovery details belong in focused documents rather than
the main README.

The visible repository will contain:

- `README.md` with a concise, human narrative and verified commands.
- `LICENSE` containing Apache License 2.0.
- `SECURITY.md` with supported-version and private-reporting guidance plus the
  plaintext-state and same-user trust boundaries.
- `CONTRIBUTING.md` with setup, TDD, style, and validation instructions.
- `CODE_OF_CONDUCT.md` using the Contributor Covenant.
- `CHANGELOG.md` with an unreleased V1 entry and no fabricated release date.
- `docs/architecture.md` containing the curated V1 architecture and transaction
  contracts.
- `docs/providers.md` containing provider setup and checkpoint details.
- Complete Cargo package metadata for a future crates.io release, including the
  Apache-2.0 identifier and canonical repository URL.

The `docs/superpowers` planning tree will be removed from the visible result.
No release, package publication, container image, logo, website, or V2 feature is
part of this work.

## GitHub Community Files

The repository will include a concise pull-request template, structured bug and
feature issue forms, and an issue-template configuration that directs security
reports to `SECURITY.md`. Discussions, wiki, and projects will remain disabled
until there is a demonstrated need. Issues will be enabled for eventual public
use.

## Security and Supply Chain

Every workflow will declare least-privilege permissions and use immutable full
commit SHAs for third-party actions. The existing Linux and macOS build matrix
will continue to run formatting, clippy with warnings denied, all tests, the two
North Star suites, and documentation.

Additional deterministic checks will cover:

- RustSec advisories against the committed `Cargo.lock`.
- Dependency licenses, sources, bans, and advisories through a checked-in
  `deny.toml` policy.
- Full-history and worktree secret scanning before the first push.
- Dependabot updates for Cargo dependencies and GitHub Actions.

No workflow receives write permissions, repository secrets, provider tokens, or
provider-authenticated test jobs. Actions and scanner versions must be verified
against their official upstream sources before pinning.

## GitHub Repository Settings

Create `thomasindrias/sesh` as private with `main` as the default branch. Enable
issues, delete merged branches automatically, and use conservative merge
settings. Protect `main` with a repository ruleset that requires a pull request,
passing required checks, resolved review conversations, and linear history while
blocking force pushes and deletion. The initial solo-maintainer configuration
requires zero approving reviews so it remains mergeable without weakening the
other gates.

Enable vulnerability alerts, automated security updates, private vulnerability
reporting, and secret push protection when supported by the account and private
repository plan. If a GitHub feature is unavailable, preserve the repository and
workflow fallback, do not silently substitute a weaker setting, and report the
exact limitation.

## Publication Flow

1. Harden and validate `feat/v1-foundation` locally.
2. Scan the full reachable history and current tree for secrets.
3. Create the private GitHub repository without auto-generated files.
4. Add the canonical `origin` remote.
5. Push the existing `main` branch, set it as the remote default, and apply the
   repository settings and ruleset.
6. Push `feat/v1-foundation` with tracking.
7. Open a draft pull request into `main` that explains the product, security
   posture, developer impact, and exact validation evidence.
8. Verify repository visibility, remote URLs, rendered README, workflow files,
   required checks, ruleset, security settings, branches, and pull request.

No merge or automatic merge is authorized. The user retains the final review and
merge decision.

## Failure Handling

Repository creation is idempotent only when an existing `thomasindrias/sesh`
repository is private and clearly belongs to this local project. A conflicting
repository, authentication failure, failed secret scan, or failed release gate
stops publication before pushing further state.

If a push succeeds but a later GitHub setting cannot be applied, keep the
repository private, finish all safe settings that are independent, and report
the precise outstanding limitation. Never make the repository public as part of
this task.

## Acceptance Criteria

The work is complete when:

- the local worktree is clean and every release/security check passes;
- no secret is detected in reachable history or the current tree;
- `thomasindrias/sesh` exists and is private;
- `main` and `feat/v1-foundation` are pushed without rewritten history;
- the protected default branch and least-privilege repository settings are
  verified through GitHub;
- a draft pull request targets `main` and contains complete validation evidence;
- the GitHub root renders a concise human README and the visible tree contains
  no internal implementation-plan directory; and
- no release, package, merge, or public visibility change has occurred.
