# In-session provider switching — development ledger

This is the execution record of the four-slice feature that made
`handover arm` / `claim` / `attach` and in-session switching work. It was
written as a working ledger during development, not as documentation, and it is
kept because slices 3 and 4 were merged by fast-forward rather than through pull
requests — so this is the only narrative account of what was decided and why.

It is verbatim apart from home-directory paths, which have been replaced with
`<worktree>`. Entries are append-only and in chronological order; later entries
correct earlier ones where a claim turned out to be wrong.

For how the system actually works today, read `docs/architecture.md` and
`docs/providers.md` — not this file.

---

# Handover arm/claim plumbing — SDD progress ledger
Plan: .superpowers/plans/2026-07-28-handover-arm-claim-plumbing.md (main repo)
Worktree: .claude/worktrees/handover-command-design
Branch: worktree-handover-command-design
Baseline: a59ee13 (origin/main, handover 0.1.1 post-rename)

Task 1: complete (commits a59ee13..fc6e27c, review clean, no findings)
Task 2: complete (commits fc6e27c..5c598f1, review clean after 1 fix round)
  - Fix: pending() had no test (Important) -> added lazy-expiry + boundary + journaled-event tests
  - Minors for final review: parse_ttl silently trims whitespace (untested/undecided);
    boundary test loops 2 cases in 1 #[test]; no idempotency test for pending() after expiry
Task 3: complete (commits 5c598f1..f2aeb37, review clean; implementer agent died pre-commit, finisher agent self-reviewed + committed)
  - Note: tests/cli_contract.rs updated here (nominally Task 6) - adding a subcommand breaks its ordered list assertion
  - --ttl default_value = crate::arm::DEFAULT_TTL path expression works; risk did not materialize
  - Minors: stale-checkpoint warn-and-proceed branch untested (FOLD INTO TASK 6); non-JSON stdout content unasserted
Task 4: complete (commits f2aeb37..40654d9, review approved after 1 coverage-fix round)
  - Fix: release_for_claim's 3 interesting lease branches were untested -> added 3 integration tests, no bugs found
  - *** DECISION FOR THOMAS (Important, plan-mandated) ***
    release_for_claim (src/app.rs:1147-1167) duplicates the live-holder determination
    from classify_lease (src/app.rs:1773-1797) almost line-for-line. My plan's Step 4
    mandated this code verbatim. Fix would be a shared helper e.g.
    fn live_holder(lease: &RunLease) -> Result<Option<&ProcessIdentity>>.
    Skill requires the human decide plan-mandated findings. NOT yet actioned.
  - Minor: foreign-host sub-case untested; dead-identity construction repeated in 3 tests
Task 5: complete (commits 40654d9..e80a38e, review approved after 1 fix round)
  - Fix 1: real TOCTOU race - classify_lease ran BEFORE SessionOperationLock. Reordered to match
    claim_command precedent. This was a genuine bug in the plan's own code.
  - Fix 2: added attach_succeeds_when_the_session_has_only_a_stale_lease (recoverable must not refuse)
  - Minor: per-arm lock acquisition duplication (accepted, arms need different sequencing)
Task 6: complete (commits e80a38e..4298a43, review clean, no findings)
  - cli_contract.rs array verified already correct (done incrementally in Tasks 3-5), no edit
  - Concurrency test stress-run 16x, 0 flakes
  - Folded in the stale-checkpoint warn-and-proceed test from Task 3's Minor
ALL 6 TASKS COMPLETE. Proceeding to final whole-branch review.

FINAL WHOLE-BRANCH REVIEW (opus, a59ee13..4298a43, 9 commits): "Ready to merge: With fixes"
  CRITICAL #1: SwitchClaimed has no through_sequence AND claim's emitted handover names a
    transition checkpoint that does not exist (preview_handover fabricates through_sequence+1,
    which collides with the switch.claimed event itself). Verified by hand against a real journal.
    Expensive to undo post-merge: append-only + deny_unknown_fields. PLAN BUG.
  IMPORTANT #2: release_for_claim clears a stale lease with no RunRecovered event (every other
    lease-clearing path journals one; architecture.md:137 claims it does).
  IMPORTANT #3: "the arming run" is whoever held the lease at arm time, not the caller ->
    arm+claim is an unprompted switch --recover-lease. SPEC WORDING FIX (mine).
  IMPORTANT #4: live-holder logic now in THREE copies (reviewer's independent answer to the
    deferred plan-mandated question: resolve it, by moving to src/store/lease.rs).
  IMPORTANT #5: claim releases the lease BEFORE it knows the handover renders.
  IMPORTANT #6: foreign-host case gets a message asserting something false.
  Minors triaged: most dismissed; #6 (dead_identity helper) folded into the fix.

FINAL FIXES: complete (commits 4298a43..96731bf)
  d0c37c4 fix: claim commits a real transition checkpoint (CRITICAL #1)
    -> Transition::{committed,hypothetical} + shared commit_transition_handover now the single
       provider-boundary path for BOTH switch_command and claim_command. SwitchClaimed gained
       through_sequence. New test the_handover_claim_emits_names_a_transition_checkpoint_that_exists
       FAILS against pre-fix src/ with left:"switch.claimed" right:"checkpoint.created".
  2632f08 refactor: live_holder extracted to src/store/lease.rs, 3 copies -> 1 (#4)
  68214a5 fix: render-then-release ordering + RunRecovered journaled + accurate foreign-host msg (#2,#3,#5)
  96731bf test: dead_identity() helper (#6)
  Controller verified: 26/26 test binaries ok, 271 passed, 0 failed, fmt+clippy clean.
  Spec amended for Important #3 ("the arming run" wording) - not a code change.
STATUS: 13 commits, all gates green. Awaiting Thomas's merge decision.

SELF-REVIEW (controller, 4298a43..96731bf - the fix commits nothing else had reviewed):
  Transition refactor verified sound: switch_command ordering preserved, from_provider unaffected
  (SwitchRequested + transition checkpoint both append with provider=None).
  live_holder verified semantically identical, and picked up a 4th call site (LeaseStore::create).
  FOUND + FIXED: SwitchClaimed's new field was named `through_sequence` but assigned the
  transition checkpoint's EVENT sequence - a pointer, not a prefix. CheckpointCreated.through_sequence
  means the prefix; the two differ by one. Renamed to transition_checkpoint_sequence, matching
  SessionForked.parent_checkpoint_sequence. Commit 5e1dbae. Same permanence argument as the
  Critical: append-only + deny_unknown_fields freezes the name.
SHIPPED: PR #14 https://github.com/thomasindrias/handover/pull/14 (14 commits, 10 files)

=== WORKTREE RETAINED FOR SLICE 2 (Thomas, 2026-07-29) ===
Worktree: <worktree>
Local branch: worktree-handover-command-design @ 5e1dbae
Pushed as: feat/switch-arm-claim-plumbing -> PR #14 (open, unmerged)

Slice 2 = Porcelain. From the spec:
  - `switch` recomposed as arm + claim + launch (behaviour unchanged from the user's view)
  - supervisor claims a pending arm on child exit -> delivers CLI->CLI in-session handover
  - docs/architecture.md + docs/providers.md updates land HERE (deferred from slice 1)
Slice 2 should also pick up, from the final review's Minors:
  - move the fake-provider bash fixture into tests/support/ (now duplicated 4x across
    arm_claim.rs and switch_readiness.rs)
  - decide whether recomposed `switch` still emits switch.requested (it does today;
    arm/claim do not) - journal-visible either way
  - envelope `provider` field is inconsistent across the 3 switch events:
    switch.requested=None, switch.armed=from-provider, switch.claimed=to-provider. Pick one.
  - consider `pending(store, runtime, events, _lock: &SessionOperationLock)` so the
    locking contract is type-enforced rather than doc-comment-enforced

CAUTION: branching slice 2 off this branch stacks it on unmerged work. If PR #14 takes
review changes, slice 2's base moves. Prefer waiting for merge, or rebase deliberately.

--- UPDATE 2026-07-29, after merge ---
RESOLVED: the CAUTION above is obsolete. PR #14 merged to main as 7727754 (squash).
PR #15 merged as 2e6c697 (Homebrew formula identifiers - unrelated, no conflict).
This worktree was reset --hard onto origin/main and is now AT 7727754, zero divergence.
Slice 2 starts from merged main. Nothing is stacked on unmerged work.
Slice 1 shipped: arm/claim/attach + src/arm.rs + tests/arm_claim.rs, 26/26 test binaries
green on merged main, `handover --help` lists arm/claim/attach.

=== SLICE 2 (porcelain) — SDD execution ===
Plan: .superpowers/plans/2026-07-29-handover-switch-porcelain.md (main repo)
Baseline: 7727754 (merged main, slice 1 included)
Decisions from Thomas: (1) switch REFUSES a pending arm for a different provider;
(2) arm emits switch.requested, claim emits switch.claimed, switch emits neither directly;
(3) envelope provider = None on all switch events.
S2 Task 1: complete (commits 7727754..74d7780, review clean)
  - Minor: new test doesn't assert SwitchRequested payload `from` (code verified correct by reviewer)
S2 Task 2: complete (commits 74d7780..ff20a5a, review clean)
  - Minor (plan-mandated): switch re-reads store.events() for the pending-arm check
S2 Task 3: complete (commits ff20a5a..adf54ad) — awaiting review
  - FOR FINAL REVIEW: implementer found the handshake idempotency key is `handshake:{native_session_id}`,
    NOT scoped per run or provider. Two providers in one session sharing a native session id would
    collide. Surfaced as a test-fixture workaround (fake_codex uses "codex-native") but the underlying
    scoping looks like a real production gap. Worth an independent judgement.
  - RESOLVED OUT OF BAND: the handshake idempotency-key defect was spun off as its own session
    and is now handover#16. It touches src/app.rs (hook handling, ~2700s) and src/store/journal.rs;
    slice 2 touches src/app.rs ~700-1400, so a conflict is unlikely but check on merge.
S2 Task 3: fix committed (db18573) — reuse-branch test added, re-review pending
S2 Task 3: complete (commits ff20a5a..db18573, review approved after 1 fix round)
  - Fix: reuse-branch (switch reusing a same-provider arm) was untested -> test added, no bug found
  - All 3 branches of the pending-arm match now covered
  - Controller resolved reviewer's warning: finished_session() runs `run claude` to completion and
    the supervise tail clears the lease, so no dangling lease blocks the non-interactive arm->switch
  - Minors for final review: switch calls resolve_switch_snapshot twice per invocation (plan-mandated);
    arm_for_switch/claim_pending have inconsistent param order; a comment references "Task 2"
S2 Task 4: complete (commits db18573..bf6e1b3, review approved after 1 fix round)
  - Fix 1: failure isolation was documented but NOT implemented (claim_pending?/resolve_saved_cwd?
    propagated). Now gated on preview_handover; Err reports on stderr and returns the finished run's
    exit code. Test verified to FAIL pre-fix.
  - Fix 2: provider_args leaked from one provider's launch into the successor's. Moved into
    LaunchRequest; a claimed hop gets Vec::new(). Test verified to FAIL pre-fix.
  - Minors for final review:
    (a) resolve_saved_cwd runs AFTER the claim, so on failure there the stderr advice
        ("armed switch is still pending, run handover claim") is WRONG - the arm was consumed.
        Narrow TOCTOU; same exposure already exists in claim_command. Fix = thread the cwd
        through BuiltHandover, or soften the message.
    (b) run now creates the run dir before adapter setup/probe -> orphan run dir if provider missing
    (c) fork_command still carries a THIRD copy of the supervise tail (out of brief scope)
    (d) no hop limit on the handover loop (deliberate: each hop needs its own arm)
S2 Task 5: complete (commits bf6e1b3..eab22e5, approved after 1 fix round)
  - Fix: "attached provider" meant TWO OPPOSITE things on one page - the established term means a
    provider Handover LAUNCHED (has HANDOVER_RUN_ID, uses the run-scoped inbox), while `handover
    attach` means one it did NOT launch. Disambiguated in providers.md + one adjective in
    architecture.md. Verified: a human CAN still write a checkpoint for an attach-bound session.
  - Minor: architecture.md:138 "stale leases are recovered explicitly" now slightly overstates,
    since a claim-triggered recovery skips the [y/N] prompt (reconciled by adjacent new prose)
ALL 5 SLICE-2 TASKS COMPLETE. Proceeding to final whole-branch review.

FINAL WHOLE-BRANCH REVIEW (opus, 7727754..eab22e5): "With fixes"
  IMPORTANT 1: a successor that cannot launch (e.g. codex not installed) consumed the arm AND lost
    the finished run's exit code - the gate covered render failures but not launch failures.
  IMPORTANT 2: switch armed BEFORE proving the handover renders, so a failed switch left a pending
    arm behind. switch was the only command mutating before its gate.
  IMPORTANT 3: switch_readiness could not see pending arms -> status reported ready:true and
    suggested a command this branch refuses.
  IMPORTANT 4: consent gate untested on the arm-REUSE path.
  Reviewer confirmed: layering holds, consent-gate ordering survived, lock discipline correct,
  journal backward-compatible with slice-1 sessions (checked every reader of envelope `provider`).
FINAL FIXES: 7c30b88 (findings 1-5) + daeaca6 (docs, finding 6)
  - Target setup+probe moved into the gate before the claim
  - preview_handover gate added to switch_command
  - switch_readiness gained an `armed` block, read via the PURE arm::is_live_at (status holds no
    lock, so arm::pending would have written without one)
  - resolve_saved_cwd moved above the claim so the stderr advice is unconditionally true
  - 5 tests added; findings 1/2/3 verified to FAIL pre-fix with the predicted symptoms
  - Known/accepted: switch still appends git.snapshot before the gate (observational only)
  Controller verified: 27/27 test binaries ok, clean tree.

=== SLICE 3 (experience layer) — GROUNDING, not yet planned ===
Worktree reset onto merged main c71c472 (slices 1+2 and the handshake fix #16 all landed).

Decisions from Thomas:
  (1) An attached CLI provider reaches arm via the CLI, mirroring provider checkpoints:
      "$HANDOVER_HOOK_BIN" arm <provider> --from-provider, gated by the same
      session-id + run-id + inbox check checkpoint --from-provider already passes.
      => provider_command_allowed gains Arm/Claim under from_provider: true.
  (2) Slice 3 INCLUDES the attach MCP tool (spec prose governs over the slice list).
      Note attach needs a SECOND authorization rule: worktree-scoped, not run-scoped,
      because a desktop app has no run env vars at all.

Findings from grounding (each would have broken a naive plan):
  - Handover does NOT auto-register its MCP server. docs/mcp.md has the user configure it
    by hand, so a slash command cannot assume MCP exists. This is why (1) is the CLI path.
  - Codex's disk-scanned surface is SKILLS, not prompts: $CODEX_HOME/skills/<name>/SKILL.md
    with YAML frontmatter (name, description). Verified against codex 0.145 and the real
    ~/.codex/skills/bokio/SKILL.md. The binary's prompts/ strings are MCP protocol.
  - OPEN QUESTION for Thomas: the private per-run CODEX_HOME symlinks only config.toml and
    auth.json, so a Handover-launched Codex session ALREADY sees none of the user's own
    skills. Adding Handover's skill there makes that gap concrete. Preserving user skills
    means symlinking each entry of the real skills/ individually beside Handover's.

Scope: run-scoped CLI arm/claim from provider processes; Claude plugin command asset;
Codex skill asset; MCP tools arm/claim/attach.

  (3) RESOLVED: preserve the user's own Codex skills. Symlink each entry of the real
      skills/ individually into the private CODEX_HOME beside Handover's own.
      Plan must decide: name collision (Handover's handover-switch must win) and
      degradation for a large/deep/broken-symlink skills dir (must not fail the launch).

=== HANDOFF: slice 3 is grounded and decided, NOT planned. Start it in a FRESH session. ===
Read, in order:
  1. .superpowers/specs/2026-07-28-handover-command-design.md  <- design, now current
       ("### Experience" is slice 3; it carries all three decisions above)
  2. this ledger, from "=== SLICE 3" upward for context on how 1 and 2 were run
  3. .superpowers/plans/2026-07-29-handover-switch-porcelain.md as a shape reference
Next step is superpowers:writing-plans for slice 3, then subagent-driven-development.
Worktree: .claude/worktrees/handover-command-design, branch worktree-handover-command-design,
reset onto c71c472 (= origin/main), clean.
Slice 3 scope: run-scoped CLI arm/claim from provider processes (provider_command_allowed gains
Arm/Claim under from_provider: true); Claude plugin command asset; Codex skill asset +
user-skill preservation; MCP tools arm/claim/attach (attach needs its own worktree-scoped rule).

=== SLICE 3 (experience layer) — SDD execution ===
Plan: .superpowers/plans/2026-07-31-handover-experience-layer.md (main repo)
Baseline: c71c472 (merged main, slices 1+2 and the handshake fix #16 included)
7 tasks: (1) run-scoped CLI arm/claim from a provider; (2) Claude /handover-switch command
asset; (3) Codex handover-switch skill asset; (4) preserve the user's own Codex skills;
(5) MCP arm/claim/attach; (6) docs; (7) armed_run narrowing.

Decisions the plan made that the spec left open:
  - Collision: a user skill named handover-switch is SKIPPED, Handover's wins, and the
    shadowing is reported once on stderr rather than silently.
  - Degradation: link_user_skills returns () (not Result) so it CANNOT fail a launch;
    one level deep, capped at MAX_LINKED_USER_SKILLS = 256, entry types read from the
    dirent so a dangling symlink is classified rather than erroring.
  - Asset immutability constraint discovered during grounding: materialize_immutable
    REFUSES a byte-changed existing asset and check_integrations maps that to
    integration.invalid with NO repair command. So assets may only be ADDED to
    integrations/<provider>/1/, never edited. This slice adds only.
  - armed_run narrowing (spec "### Claim"): initially deferred, then ADDED AS TASK 7 at
    Thomas's request. Two consequences the plan works out:
      (a) release_for_claim's refusals must be REORDERED to host -> liveness -> ownership.
          With out-of-run arms recording armed_run: None, a live foreign lease would
          otherwise hit the ownership refusal first and be told "not created by the run
          that armed the switch" instead of the actionable "quit it".
      (b) claim_clears_a_dead_lease_left_by_the_arming_run_without_prompting keeps its
          name and every assertion, but must arm from INSIDE the run. It does that by
          speaking as the finished session's REAL run (run dirs outlive their run; only
          `handover delete` removes them), so the 0700 inbox chain active_run validates
          already exists. New helper: run_dir_and_id().
      (c) claim_refuses_while_the_arming_runs_provider_is_still_live is renamed to
          claim_refuses_while_a_provider_is_still_live — body unchanged, it is the guard
          for the reordering in (a).
S3 Task 1: complete (commits c71c472..fd0078f, review approved after 1 fix round)
  - Base 36760b4 reviewed: spec EXACT, quality approved w/ findings. Fix fd0078f closed 3 Important.
  - Fix 1: claim --from-provider's gate had NO mutation-detecting test (deleting it left the suite
    green - a bare claim fails anyway with a different message). Five refusal cases now paired with
    the message that proves the intended guard fired.
  - Fix 2: the cross-session test discarded stderr (2>&1 >/dev/null), so any unrelated refusal
    satisfied it. Now captures stderr to a file and asserts authorize_run_scoped_write's wording.
  - Fix 3 *** DESIGN CHANGE, THOMAS DECIDED ***: active_run proves a run EXISTED, not that it is
    CURRENT (run dirs outlive their run), so a stale environment could arm a switch the user never
    asked for. authorize_run_scoped_write now also requires the caller's run to still hold the
    session's lease. Liveness deliberately NOT required (supervisor killed + child alive must still
    be able to hand over). DEPARTS from the spec's "same session-id + run-id + inbox check" wording.
    Plan's Task 6 docs step updated to cover it.
  - Both mutation checks reproduced independently by the re-reviewer, not just trusted.
  - Minors for final review:
    (a) authorize_run_scoped_write's doc comment still describes only the session proof, not the
        lease requirement -> FOLD INTO TASK 7 (which already edits that function's signature)
    (b) no test for the POSITIVE half of the liveness decision (dead-but-owned lease still arms)
        -> Task 7's claim_clears_a_dead_lease... does exactly this; verify it lands
    (c) authorize_run_scoped_write runs BEFORE SessionOperationLock, so the lease read is a narrow
        TOCTOU. Pre-existing pattern; the fn disclaims being an authorization boundary.
    (d) fake_claude_that_only_checkpoints duplicates fake_claude minus one line (test-file DRY)
S3 Task 2: complete (commits fd0078f..520c056, review clean, no fixes needed)
  - Asset bytes diffed byte-for-byte against the brief by the reviewer (1853 bytes, identical)
  - setup/verify symmetry checked: both list the same 4 claude assets in the same order
  - CLI surface of the asset's prescribed commands verified against src/cli.rs (right flags,
    right positional, Provider ValueEnum lowercases to claude|codex matching argument-hint)
  - Minors for final review (both plan-mandated):
    (a) assert!(!text.contains("mcp")) is a lexical proxy for a semantic invariant; a harmless
        future footnote mentioning MCP would trip it
    (b) the new test does install-content AND the remove/verify-fails/setup-restores cycle in one
        fn, where the checkpoint equivalents are two separate tests
  - Brief prose inaccuracies found by the implementer (harmless, no code impact): said "five"
    claude adapter tests (six after this) and pointed at tests/doctor.rs for the outdated-asset
    regression test, which actually lives inline in src/doctor.rs's mod tests
S3 Task 3: complete (commits 520c056..14a6c94, review approved after 1 fix round)
  - *** CRITICAL, PLAN BUG (mine) *** the brief said "update the one call site in launch_spec".
    There are THREE production/test call sites of materialize_codex_home whose 2nd param changed
    MEANING (hooks.json file -> integrations/codex/1 version dir) while staying &Path, so the
    compiler could not catch the two that were missed. Result: `handover setup codex` built a
    review CODEX_HOME with DANGLING hooks.json and skills symlinks - the user is told to open
    /hooks and trust what they see, and there were no hooks at all, exit code still the designed 2.
    Reviewer proved it by running the built binary, not by reading. Fixed in 14a6c94.
  - Fix 2: src/doctor.rs's permissions test had the same stale call site -> was green while
    testing a malformed fixture (the permission walk stops at codex_home via is_provider_owned_home)
  - Fix 3: nothing asserted the setup review home was USABLE (only that stdout said CODEX_HOME=).
    That gap is why the Critical shipped green. Now asserts hooks.json and SKILL.md read back
    byte-equal THROUGH the symlinks; verified to fail pre-fix with NotADirectory.
  - Fix 4: setup/verify hardcoded "skills/handover-switch/SKILL.md" while HANDOVER_SKILL existed
    -> same bug class. Both now build from the constant; path string verified byte-identical so
    existing content-addressed installs are not orphaned.
  - refresh_symlink on a symlink-to-DIRECTORY verified correct (remove_file/unlink never follows).
    Fails only against a real directory at the link path, which prepare_run_directory makes
    unreachable (it refuses an existing run dir).

*** EMPIRICAL FINDING (controller, codex 0.145, quota-free via `codex debug prompt-input`) ***
Probed a scratch CODEX_HOME holding one real skill dir and one symlinked skill dir:
  1. Codex DOES follow a directory symlink - `linkdir-skill` appears in the model-visible prompt
     alongside `realdir-skill`. This settles the reviewer's "cannot verify from diff" risk that
     the whole Task 3/4 design rests on. Directory symlinks are safe.
  2. Codex WRITES its own built-ins into $CODEX_HOME/skills/.system on every start (6 skills +
     a .codex-system-skills.marker), and the user's real ~/.codex/skills has a .system too.
     => Task 4 MUST NOT link .system: Codex would refresh its built-ins THROUGH the symlink into
     the user's real ~/.codex, violating the private home's whole guarantee. Plan patched to skip
     every dot-entry, with a dedicated test.
  (`codex mcp-server` advertises only tools, no prompts capability - the skills surface is not
   reachable over MCP, confirming the earlier grounding note.)
S3 Task 4: complete (commits 14a6c94..b7dfc19, review approved after 2 fix rounds)
  - *** REAL BUG, found by review, reproduced on real APFS ***: the collision check was
    byte-exact (name == HANDOVER_SKILL). On macOS's case-folding APFS - this project's own
    platform - a user skill named `Handover-Switch` failed that check, fell through to
    refresh_symlink, whose symlink_metadata resolved case-insensitively ONTO Handover's existing
    link, remove_file deleted it, and symlink replaced it. The user's content ended up served at
    handover-switch, SILENTLY. Both halves of the collision decision were defeated.
    Fixed two complementary ways: (1a) eq_ignore_ascii_case so the skip + warning fire;
    (1b) Handover's own refresh_symlink moved to run AFTER the user walk, so it holds that path
    structurally regardless of how any filesystem folds names.
  - Fix 2: the `|| kind.is_symlink()` arm was load-bearing and untested - a symlink to a real dir
    reports is_dir()==false, so that arm is the ONLY reason `~/.codex/skills/foo -> elsewhere`
    (a common setup) links at all. Deleting it broke nothing. Test added, mutation-verified.
  - Fix 3: the non-NotFound read_dir branch (skills/ is a regular file, or mode 000) was untested;
    an unwrap there would pass every test and panic a launch. Tests added, mutation-verified.
  - Fix round 2: 1b alone made every CONTENT assertion pass even with 1a reverted, so the WARNING
    half could silently regress. The integration test's colliding fixture was renamed to the
    case-variant `Handover-Switch`, which fails under either mutation (== restored, or eprintln
    deleted) and behaves identically on case-folding and case-sensitive filesystems.
  - Verified: link_user_skills still returns () with no `?`; the 1b reordering is safe for the
    PERSISTENT `handover setup codex` review dir across repeat invocations.
  - Minors for final review:
    (a) entries.flatten() and the `_ => continue` type-filter both drop per-entry errors with no
        warning, where the outer dir-open failure warns ("fewer skills, one warning" is the stated
        contract; these two paths give fewer skills and no warning)
    (b) link_user_skills only adds, never prunes, so the persistent review dir's skill set only
        grows - a user skill later deleted leaves a stale link. Pre-existing structure.
    (c) `handover setup codex` run from INSIDE a Handover-launched Codex session prints a spurious
        "your own handover-switch skill is shadowed" warning, because resolve_provider_home prefers
        $CODEX_HOME, which then points at the private home that already holds Handover's link.
S3 Task 5: complete (commits b7dfc19..b958ce6, review approved after 1 fix round)
  - MCP arm/claim/attach shipped; layering contract test (the_full_cycle_runs_on_the_cli_alone...)
    passes UNTOUCHED. Lock lifetime verified against pre-change code: every journal write still
    happens inside SessionOperationLock; only output formatting moved outside it.
  - Fix 1: the claim half of the_write_tools_refuse_outside_the_active_run proved NOTHING -
    reviewer removed authorization from mcp_claim_value entirely and it stayed green, because the
    fixture never armed anything so claim failed at "no switch is armed" regardless. Now arms via
    the CLI first and asserts the error TEXT ("HANDOVER_SESSION_ID is required"), which is what
    discriminates: with authorization removed, arm fails with "already armed" - isError is STILL
    true, so only the text assertion catches it. Both mutations reproduced by the re-reviewer.
  - Fix 2 (structural, not a test): mcp_claim_value re-assembled the projection through its own
    9-arg build_handover_value call, with two same-typed pairs (transition_sequence/through_sequence,
    from_provider/to_provider) that could transpose and still compile. Extracted claim_projection
    so there is now exactly ONE call site. CLI --json output verified unchanged.
  - Fix 3: neither write tool had success-path coverage. New integration test has the FAKE PROVIDER
    pipe JSON-RPC into "$HANDOVER_HOOK_BIN" mcp-server - the realistic fixture, and the only state
    where these tools are authorized (live run + live lease). Mutation-verified: ignoring surface,
    ignoring ttl, or dropping claim's arm argument each fail it.
  - Fixes 4-6: contradictory comment resolved; required:["provider"] asserted for the new tools'
    schemas; claim_core's doc now states callers must not write through the returned SessionStore.
  - STRUCTURAL LIMITATION (accepted, documented): claim's SUCCESS path over MCP is unreachable by
    construction - the server is spawned by the provider holding the lease, and a claim refuses
    while that lease is live; once the provider exits there is no server to call. The MCP claim
    tool is effectively decorative until slice 4's attach-tier sessions, whose lease is free.
    The CLI covers the success path and claim_core is shared verbatim.
  - Minors for final review:
    (a) src/mcp.rs advertises "Defaults to 15m." as prose while the code reads arm::DEFAULT_TTL;
        nothing binds them
    (b) the tools' inputSchema enums (["claude","codex"], ["auto","cli","desktop"]) are hand-typed
        in 3 places with nothing tying them to Provider/Surface - adding a variant ships a stale
        contract to agents silently
    (c) tests/mcp_server.rs duplicates a fake-claude fixture that run_fake_claude() already provides
    (d) arm_value still eprintln!s the stale-narrative warning, which on the MCP path goes to the
        server's stderr where no agent sees it (the same signal is in the checkpoint_fresh field)
S3 Task 6: complete (commits b958ce6..964fe83, review approved; 1 Minor fixed by controller)
  - The "pure reads with no mutation path" justification for the McpServer guard exception is
    replaced, not deleted: architecture.md now states the three-gate run scoping for arm/claim and
    the deliberate worktree scoping for attach, and says plainly it is a guardrail not a boundary.
  - Implementer correctly DEVIATED from the brief twice: my prose said provider writes "add nothing
    to" the checkpoint path's scoping, which is false since Task 1's lease requirement - checkpoint's
    active_run has NO lease check, so provider arm/claim writes are strictly MORE scoped than
    provider checkpoint writes. Prose corrected in two architecture.md paragraphs.
  - Reviewer verified every documented tool name, schema, and CLI flag against src/, and confirmed
    no sentence claims an agent can complete a switch over MCP today.
  - Controller fixed 1 Minor: providers.md said a missing skills/ dir costs "a warning on stderr",
    but link_user_skills returns silently on NotFound ("the ordinary case, not a problem").
    Commit 964fe83.
  - Minor for final review: docs/mcp.md's scoping paragraph describes two gates, not three; it
    defers to architecture.md so it is not false, but a reader of mcp.md alone misses the
    dead-but-owned-lease nuance.
S3 Task 7: complete (commits 964fe83..06af8fc, review APPROVED, no Critical/Important)
  - armed_run is now recorded only when the caller IS the run holding the lease. An arm typed in a
    plain terminal adopts nothing, so `arm && claim` can no longer release a crashed run's lease
    without the consent prompt `switch --recover-lease` requires.
  - release_for_claim's refusals reordered host -> LIVENESS -> ownership.
  - *** PLAN DEFECT (mine), caught by the implementer and confirmed by the reviewer ***: my brief
    claimed claim_refuses_while_a_provider_is_still_live was "the guard for the reorder" and would
    fail without it. FALSE - with the reorder reverted it still passed, because classify_lease
    returns "{provider} is still running this session (...)" for a live lease and the OWNERSHIP
    refusal appends that same reason, satisfying both contains() assertions via the wrong message.
    Implementer added a negative assertion (!contains "was not created by the run that armed the
    switch"); reviewer independently reproduced that it is the sole failure under the old ordering.
  - Implementer correctly REFUSED a stale instruction: the brief's Step 5 quoted authorize_run_scoped
    _write without the lease check added in Task 1. Applying it verbatim would have silently dropped
    that gate - a security regression. They kept it and folded in the missing doc-comment coverage.
  - Reviewer verified: the consent gate is strictly MORE protected (the narrowing computes a strict
    subset; reordering refusals cannot admit a case); switch_command still records armed_run: None
    for a stronger reason than the explicit argument (the lock is held from recovery through arming,
    so no lease can appear in between); and the caller_run match is NOT dead logic - it is the
    decisive re-read UNDER SessionOperationLock, which authorize_run_scoped_write does not hold.

=== ALL 7 SLICE-3 TASKS COMPLETE (c71c472..06af8fc, 12 commits) ===

MINORS ACCUMULATED ACROSS THE SLICE — for the final whole-branch review to triage:
  T1(a) authorize_run_scoped_write's doc comment lacked the lease requirement — FIXED in Task 7
  T1(b) no test for the POSITIVE half of the liveness decision — COVERED by Task 7's
        claim_clears_a_dead_lease_left_by_the_arming_run_without_prompting
  T1(c) authorize_run_scoped_write reads the lease BEFORE SessionOperationLock (narrow TOCTOU,
        pre-existing pattern; Task 7's reviewer notes the consequence is now narrowed)
  T1(d) fake_claude_that_only_checkpoints duplicates fake_claude minus one line
  T2(a) assert!(!text.contains("mcp")) is a lexical proxy for a semantic invariant
  T2(b) one test does install-content AND the upgrade cycle, where checkpoint's are two tests
  T4(a) entries.flatten() and the `_ => continue` type filter drop per-entry errors with no warning,
        against a stated "fewer skills, one warning" contract
  T4(b) link_user_skills only adds, never prunes, so the PERSISTENT setup review dir only grows
  T4(c) `handover setup codex` run from inside a launched Codex session prints a spurious
        "your own handover-switch skill is shadowed" warning (resolve_provider_home prefers
        $CODEX_HOME, which then points at the private home that already holds Handover's link)
  T5(a) src/mcp.rs advertises "Defaults to 15m." as prose while the code reads arm::DEFAULT_TTL
  T5(b) the tools' inputSchema enums are hand-typed in 3 places, untied to Provider/Surface
  T5(c) tests/mcp_server.rs duplicates a fixture run_fake_claude() already provides
  T5(d) arm_value's stale-narrative eprintln goes to the MCP server's stderr where no agent sees it
  T6(a) docs/mcp.md's scoping paragraph describes two gates, not three (defers to architecture.md)
  T7(a) claim_refuses_when_a_different_run_holds_the_lease silently degraded from Some(A) vs Some(B)
        to None vs Some(B); its comment "Its run id was never seen by arm" is now FALSE, and NO test
        anywhere exercises the ownership refusal with a non-None armed_run
  T7(b) docs/architecture.md:168 is 133 chars in a file wrapped at <=80
  T7(c) CHANGELOG's "### Changed" documents a narrowing of arm/claim, which are themselves unreleased
        in the same "### Added" block — should fold into the Added bullet before cutting a release
  T7(d) arm_for_switch's caller_run match doesn't say WHY it isn't redundant (the lock boundary)
  T7(e) release_for_claim's "Two separate refusals" comment now sits above a three-rung ladder

FINAL WHOLE-BRANCH REVIEW (opus, c71c472..06af8fc, 13 commits): "Ready to merge with fixes"
  Verified holding: the three authorization paths compose and are strictly ordered by strength
  (worktree ⊂ active_run ⊂ authorize_run_scoped_write); the pre-lock TOCTOU is benign because
  arm_for_switch re-reads the lease UNDER the lock and that read is decisive; NO journal change at
  all (armed_run is the existing envelope run_id, no new event kind or payload field, nothing for
  deny_unknown_fields to reject); old installs self-repair because launch_supervised_run calls
  adapter.setup() on every launch; the layering contract holds (src/mcp.rs is argument parsing only).

  IMPORTANT #1: BOTH new provider assets prescribed a checkpoint JSON whose schema they never gave,
    and the shape they described is REJECTED. NarrativeInput is deny_unknown_fields with NO
    serde(default), so all ten keys are required, and serde reports one missing field at a time --
    a model following the instruction literally needs up to SEVEN failed shell round-trips. Worst
    for Codex: there is no Codex checkpoint skill for the switch skill to defer to, and the
    first-run document carries no schema either, so `handover run codex` -> "switch me to claude"
    (the flagship path of this slice) started with a guessing game. PLAN BUG (mine).
  9 Minors, all triaged "fix before merge" except 6 deferred and 5 dropped.

FINAL FIXES: 9dfa3cc (all 10 in one commit)
  - Both assets now inline the ten-field template from src/handover.rs's CHECKPOINT_INSTRUCTION, so
    all three sources agree byte-for-byte, plus the error-recovery line and a quoted <provider>.
  - Minor 2 fixed ONE LEVEL UP from where the review pointed: the fixer found config.toml and
    auth.json self-reference identically when `handover setup codex` is re-run inside its own review
    home (source == target -> ELOOP), so the guard sits on the block owning all three links.
    This also subsumes accumulated minor T4(c).
  - Minor 11: claude.rs now routes both command assets through constants, matching what Task 3's
    Fix 4 established for codex.rs; path strings verified byte-identical so no install is orphaned.
  Controller verified INDEPENDENTLY: extracted the template verbatim from the shipped codex asset,
  filled its placeholders, and round-tripped it through the real binary inside a real run ->
  TEMPLATE-ACCEPTED, and the review's shortened variant -> SHORT-REJECTED.
  Gates: 307 passed / 0 failed, cargo fmt --check clean, clippy -D warnings clean.

KNOWN, ACCEPTED: editing the two assets makes a DEV install of an earlier commit of this branch
  report integration.invalid (materialize_immutable refuses byte-changed assets, and that maps to
  a diagnostic with no repair command). A RELEASED v0.1.1 install is unaffected -- it never had
  these files, so it reports the repairable integration.outdated. If you ran `handover setup claude`
  or `setup codex` from an earlier commit of this branch, delete
  $HANDOVER_HOME/integrations/{claude,codex}/1/ and re-run setup.
STATUS: 14 commits, all gates green, ready for the merge decision.

=== MERGED LOCALLY (Thomas, 2026-08-02) ===
Fast-forwarded main c71c472 -> 9dfa3cc (14 commits). Tests re-run ON THE MERGED RESULT in the main
checkout: 307 passed / 0 failed, fmt --check clean, clippy -D warnings 0.
main and worktree-handover-command-design are the SAME commit, zero divergence - the same state
slices 2 and 3 each started from, so slice 4 can begin here directly with no reset needed.
NOT PUSHED: origin/main is still c71c472; main is 14 commits ahead locally.
Worktree RETAINED (it lives under .claude/worktrees/, which the harness owns, not superpowers).
Branch NOT deleted - the worktree has it checked out and slice 4 continues from it.

=== SLICE 4 (desktop) — NOT STARTED ===
From the spec's Implementation Slices: `attach` desktop transports (`codex app <path>`,
`open claude://code/new`) and attach-tier reporting in status/list/doctor.
Two things this slice learned that slice 4 will need:
  - `codex app [PATH]` is real and takes a workspace path only (verified in `codex --help` 0.145).
  - The MCP `claim` tool's success path is UNREACHABLE today: the server is spawned by the provider
    holding the lease, and a claim refuses while that lease is live. Slice 4's attach-tier sessions
    have a FREE lease, which is what finally makes that tool reachable. Worth an explicit test then.
Deferred Minors that slice 4 or a follow-up should pick up are listed under "MINORS ACCUMULATED"
above; the final review triaged 6 as "fix later" and dropped 5.

=== SLICE 4 (desktop) — GROUNDING (controller, 2026-08-03) ===
CI on merged main 9dfa3cc: all 4 jobs SUCCESS (test+install.sh on ubuntu-latest AND macos-latest).
The ubuntu run is the one that mattered - Linux is case-sensitive and the Codex collision guard
assumes APFS folding; it passes there too.

Verified empirically (read-only, nothing launched):
  1. `codex app [PATH]` is real and official: "Launch the Desktop app (opens the app installer if
     missing)", takes a workspace PATH only, [default: .]. No session id, no prompt, no config
     injection - exactly as the spec describes.
  2. Claude.app IS installed and DOES register the `claude` URL scheme (CFBundleURLSchemes:
     ["claude"]). The literal string `claude://code/new` appears in two bundle JS files
     (ion-dist/assets/v1/c31acae93-COGG4Y3V.js and shared-16-Oa53aqjk.js), so the route is real -
     and still undocumented private surface, so best-effort with a degrade path as the spec says.

Code gaps confirmed:
  3. `Surface` is DEAD WEIGHT today: threaded cli -> arm -> journal -> PendingArm and back out,
     and nothing anywhere branches on it. Slice 4 is what makes it live.
  4. `SessionAttached {}` is WRITE-ONLY: emitted by attach_command (src/app.rs:1679), read by
     nothing. status/list/doctor cannot distinguish an adopted session, and build_status_value
     would report provider: null for one, because previous_provider reads RUN events and an
     attach-tier session has no runs.

  5. *** PRODUCT CONSTRAINT worth stating before planning *** The desktop PULL path depends on the
     user having hand-configured Handover's MCP server. Handover does not auto-register it
     (docs/mcp.md has the user do it), and a desktop app gets NO injection at all - no --plugin-dir,
     no CODEX_HOME, no HANDOVER_HOOK_BIN, no run inbox. So `codex app` / `claude://code/new` deliver
     a handover only for users who already set up MCP by hand. Attach-tier reporting, by contrast,
     helps everyone who runs `handover attach`. The two halves are separable.

CONTROLLER DECISION (not asked, forced by a standing constraint): spec Open Question 2 - an adopted
session PERSISTS as an ordinary session until deleted. Auto-deleting when its app closes would
require noticing the app closed, which needs a daemon, and "no daemon" is a standing non-goal.

=== SLICE 4 (desktop + attach tier) — SDD execution ===
Plan: .superpowers/plans/2026-08-03-handover-desktop-tier.md (main repo)
Baseline: 9dfa3cc (merged main, slices 1-3 included, pushed, CI green on ubuntu + macos)
Decisions from Thomas: (1) BOTH halves, reporting first (tasks 1-3 reporting, 4-5 transports);
(2) arm --replace SHIPS, bare arm keeps refusing.
Controller decisions: (3) an adopted session PERSISTS until deleted (no-daemon constraint);
(4) NO new event kind or payload field - tier and detachment are DERIVED from run.started /
session.attached / switch.claimed, which avoids the permanence trap that cost slice 1 a Critical.
6 tasks: (1) src/session.rs tier derivation; (2) status reports it; (3) list + doctor report it;
(4) arm --replace; (5) Surface::Desktop selects a launch transport; (6) docs.
Pre-flight self-review fixed two plan defects before dispatch: Task 3's doctor step and Task 5's
launcher step both described rather than showed code. Also established that Diagnostic has no
non-fault severity (error/warning/repaired only) and doctor's exit code keys on "error" alone, so
Task 3 adds a `note` severity that cannot change the exit code.
S4 Task 1: complete (commits 9dfa3cc..57cea0d, review approved after 1 fix round)
  - src/session.rs: Tier{Supervised,Attached} + Binding{tier,provider,sequence,detached} +
    binding(events). No new event kind, no payload field - all derived from run.started /
    session.attached / switch.claimed. Verified by the reviewer against deny_unknown_fields.
  - *** PLAN BUG (mine), caught by review ***: binding() picked tier/provider by ARRAY POSITION
    (.rev().find) but computed `detached` by SEQUENCE VALUE (event.sequence > latest.sequence) -
    two semantics in one function, and the doc comment stated the one the code did NOT implement.
    Reviewer demonstrated [attached(5), run_started(3)] returning Supervised/seq-3 when seq 5 is
    the later event. Cannot manifest today (append_optional assigns last.sequence+1 and events()
    returns append order) but nothing in the &[Event] signature enforces it, and this is explicitly
    the ONE derivation three surfaces share. Fixed to .filter().max_by_key(|e| e.sequence) in
    57cea0d, with a test verified FAILING pre-fix.
  - NOTE: the plan file still shows the pre-fix .rev().find() in Task 1's code block. The shipped
    code is correct; the plan text is stale on that one snippet.
  - Brief's Event helper assumed 5 fields; the real struct has 9 (schema_version, sequence,
    occurred_at, recorded_at, session_id, run_id, provider, idempotency_key, kind). The brief's
    Step 3 told the implementer to verify and fix the HELPER, never the type - it did.
  - Reviewer independently ran 2 more mutations beyond the implementer's: forward .find() fails
    the both-directions test, and dropping the tier guard fails the supervised-never-detached test.
    All 5 original tests load-bearing. Tier serializes lowercase as Tasks 2/3 need.
S4 Task 2: status reports the binding (commits 57cea0d..bb8a560 + fix round pending)
  Review: spec compliant. Journal-safety question answered thoroughly - the reviewer enumerated
  every reader of switch.requested by hand (arm.rs, doctor.rs, list.rs, mcp.rs, log, tests) and
  found NONE reads `from`, and `from` was already Option<Provider>, so changing null -> "claude"
  is a value change inside a declared type, not a shape change. Unlike slice 1's Critical.
  IMPORTANT 1: previous_provider threw away `detached`. After attach->arm->claim, it kept naming
    the provider the claim left behind, journaling {"from":"claude","to":"claude"} for a session
    actually on codex, and suggesting a switch to the provider just claimed. Reproduced end to end.
    Fix: consult `detached` and return Ok(None) - nothing IS bound. Restores the honest null.
  IMPORTANT 2: half the new binding block was unfalsifiable - the reviewer hardcoded
    "sequence": 0 and "detached": false in the projection and the WHOLE SUITE stayed green.
    Task 1's unit tests cover the derivation; nothing covered the WIRING, which is what Task 2 added.
    One attach->arm->claim->status test closes both findings.
  Minor 3: the new attach-tier switch target (suggested_switch_command flipping from "switch claude"
    - the provider you are already on - to "switch codex") is the user-visible payoff and was
    pinned by nothing, because every switch_readiness test builds its session with `run claude`.
  Minor 4: previous_provider's error text lost the event kind ("binding event N" vs "run.started
    event N"). bound.tier was in hand.
  Minor 5 (accepted): build_status_value derives binding() twice. Reviewer proved it cannot
    disagree (pure fn, immutable local, deterministic max_by_key). Left as-is; noted that fixing
    Important 1 inside previous_provider means status.provider and status.binding.provider now
    differ by design, which the new test asserts together so the intent is legible.
  Reviewer also confirmed fork's and build_handover's from_provider are IMPROVED by the change,
  and that completeness is carried by capture_gaps + binding.tier, not by from_provider being null.
  NOTE: the fix agent was cut off by an API error mid-run; its edits were on disk and correct.
  Resumed from transcript to finish evidence + commit rather than re-dispatching.

S4 Task 2: complete (commits 57cea0d..8bfbc59, review approved after 1 fix round)
  - Fix 8bfbc59 applied all four findings. The fix agent stalled TWICE, both times on a Bash call;
    the Bash permission classifier went unavailable around then, which is the likely cause rather
    than anything about the work. It had already committed cleanly before the second notification
    fired, so the controller verified the result instead of paying for a third context rebuild.
  - Controller verification: 316 passed / 0 failed, cargo fmt --check clean, clippy -D warnings 0.
    src/model/event.rs and src/provider/assets/ confirmed untouched; tree clean.
  - Falsifiability of the new test confirmed BY CONSTRUCTION rather than by running the mutation
    (the classifier was already unavailable): it asserts binding.detached == true, which a
    hardcoded false cannot satisfy, and binding.sequence against a value READ FROM
    `handover log --json`, which a hardcoded 0 cannot satisfy. Both of the reviewer's mutations
    are therefore caught. This is weaker evidence than an executed mutation — if anyone wants the
    stronger form later, hardcode each value in build_status_value's projection and re-run
    tests/attach_tier.rs.
  - Resulting shape is deliberate and asserted together so it reads as intended:
    status.provider == null WITH binding {provider: "claude", tier: "attached", detached: true} -
    "nothing is bound now; the last attachment was claude, and it is detached".

S4 Task 3: complete (commits 8bfbc59..ad22fb5, review approved after 1 fix round)
  - list + doctor now state the tier. Diagnostic gained a `note` severity (error/warning/repaired
    were the only three, and `warning` attaches "handover doctor --repair", which would tell the
    user to fix a supported state). doctor's exit code keys on "error" alone, so a note cannot
    fail a health check.
  - Brief was wrong about JournalScan: it has `envelopes: Vec<EventEnvelope>`, not `events`. The
    implementer used the brief's own sanctioned fallback.
  - *** PLAN BUG (mine), 3rd of this shape *** Finding 1: list reported a DETACHED attachment
    identically to a live one (bound:true, tier:attached, last_provider:claude) while status on the
    same state said detached:true, provider:null. My Step 3 snippet dropped `detached` exactly as
    Task 2's previous_provider had. Fixed: last_provider returns None when detached, and the row
    carries a "detached" member (null in degraded_row, keys stay symmetric).
  - Finding 2: the new test asserted only stdout content, never doctor's EXIT CODE - the whole
    reason `note` exists. Reviewer proved it by flipping note->error and watching the test still
    pass. Now asserts success(), verified failing under that mutation.
    NOTE: making that assertion meaningful required upgrading the shared adopted_worktree() fixture
    from --version-only stubs to capable fakes + `handover setup`, else unrelated
    provider.capability_missing / integration.missing errors would fail it regardless of severity.
  - Finding 3 (minor): closure param `bound` renamed to `binding` in the one fn where two different
    `bound`s were flagged as collision-prone.
  - DEFERRED TO FINAL WHOLE-BRANCH REVIEW: check_sessions deep-clones every event of every session
    on every doctor run purely to compute one boolean, where binding() only ever borrows. Fix is an
    iterator signature on binding(). Held back deliberately - this slice has twice shipped bugs
    where a signature or a parameter's meaning changed, so it does not belong bundled with
    behavioural fixes.
  - INCIDENT: the Task 3 implementer ran `handover attach` without HANDOVER_HOME set and wrote a
    session into the user's REAL ~/.local/state/handover. It detected and removed the stray itself.
    Controller verified: the user's own session (f2d75557, <repo>, last
    activity 2026-07-28) is intact and nothing from that day remains. One older scratchpad-path
    session (1d8a9070, 2026-08-02) is pre-existing residue, left alone - deleting user state
    unasked is not the controller's call. ALL later dispatches now carry an explicit
    "set HANDOVER_HOME to a temp dir" rule.
S4 Task 4: complete (commits ad22fb5..e674fcc, review approved after 1 fix round)
  - arm --replace ships; bare arm and `switch` both keep refusing. Supersede reuses the EXISTING
    switch.expired event (armed_sequence names the SUPERSEDED arm) - no new event kind or field.
  - Reviewer verified the append is inside the SessionOperationLock window (acquire 1369, append
    1384, lock held to return), and ran the transposition mutation: naming the NEW arm's sequence
    instead of the superseded one fails the test with left:13 right:11. Genuine mutation-catcher.
  - Fix e674fcc: the no-op path (--replace with NOTHING pending) had no test. Behaviour was already
    correct but a regression would journal a switch.expired naming an arm that never existed -
    permanently, in an append-only checksummed journal. Test added, proven to fail under a planted
    unconditional-append mutation. src/ untouched by the fix.
  - switch_command's refusal reworded to match arm's punctuation while KEEPING its remedies honest
    (no --replace on switch, so it must not advertise one). Reviewer confirmed correct.
  - *** DEFERRED TO FINAL WHOLE-BRANCH REVIEW (plan-mandated, mine) ***
    arm_command is now 8 args with THREE CONSECUTIVE BOOLS (replace, from_provider, json) and
    gained #[allow(clippy::too_many_arguments)]. A slice-3 reviewer PREDICTED this exact outcome
    and recommended a struct instead of the allow, because slice 2 shipped a real bug from a
    9-arg call with two transposable same-typed params. Today's single call site is safe only
    incidentally (it passes matching identifiers), not structurally. My brief mandated the
    positional signature, so this is my call to make, not the implementer's.
    Also noted: the report claimed a named local `let from_provider = true;` "guards" against a
    swap in mcp_arm_value. It does not - Rust has no keyword args, so a positional swap compiles
    either way. It aids review, it is not a guard. Only a struct literal or newtypes would be.
S4 Task 5: complete (commits e674fcc..c3bc5b3, review approved after 1 fix round)
  - src/launch.rs: DesktopLaunch + desktop_launch() + DesktopLauncher trait + SpawnLauncher
    (fire-and-forget, null stdio, never waited). Surface is LIVE for the first time since slice 1.
  - Test seam: a test-only capture variable (name deliberately not repeated here) -> CaptureLauncher records and opens
    nothing. Integration tests spawn the real binary, so a Rust trait cannot reach the launch; a
    fake on PATH would race the fire-and-forget spawn AND still run the real `open` for Claude.
    Reviewer accepted it: the var can only SUPPRESS a launch, never redirect it, and anyone who
    can set it can already set HANDOVER_HOME. tests/repository_contract.rs now refuses any shipped
    Markdown naming that test-only prefix (mutation-verified) - which is why this ledger does not spell it out.
  - *** IMPORTANT 1, real functional gap *** `handover switch <provider>` reused a pending arm and
    NEVER READ existing.surface -> a Surface::Desktop arm got supervised in the terminal anyway.
    Same arm, two transports depending on which command claimed it. Reviewer proved it empirically.
    My brief pointed at ONE claim path (launch_supervised_run) and there are two.
    FIXED in c3bc5b3 by extracting the Surface->transport mapping into ONE function,
    claimed_transport(), used by both switch_command and the claim-on-exit path.
  - IMPORTANT 2: "the session reads as attach tier" was pinned by NOTHING - the reviewer planted a
    RunLease in the desktop arm and all 328 tests stayed green. Now asserted via status --json.
    The fixer corrected my premise: right after a hop the session reports tier=supervised for the
    FINISHED provider (nothing has attached yet), so the test drives the attach the opened app
    would make over MCP and asserts `attached` there. Better understanding than my finding had.
  - MINOR 3: Surface::Cli had zero transport coverage (mutating Cli->Desktop stayed green). Covered.
  - Bonus from the fix: open_desktop's two same-typed Provider args collapsed into
    Option<FinishedRun>, so the transposition risk is now structural, not test-guarded.
  - EXIT CODE on the desktop path: 0 if the app opened, 1 if not. switch's code has always meant
    "how did the thing I launched end"; detached there is no end, so the honest question is "did
    the switch land". The claim-on-exit rule (preserve the finished run's code) deliberately does
    NOT apply - there the code belongs to work already done; switch has none to preserve.
  - 331 passed / 0 failed. All 3 mutations verified and restored.

MINORS FOR THE FINAL WHOLE-BRANCH REVIEW (slice 4):
  (a) check_sessions deep-clones every event of every session on every doctor run to compute one
      boolean; binding() only borrows. Fix = iterator signature. Held back deliberately.
  (b) arm_command: 8 args, THREE CONSECUTIVE BOOLS, #[allow(clippy::too_many_arguments)]. A
      slice-3 reviewer predicted this and recommended a struct. Plan-mandated (my positional spec).
  (c) The desktop success line says "Opened ..." even when the capture seam suppressed the launch.
  (d) `open` is macOS-only but README promises Linux and CI builds linux-musl -> every Claude
      desktop arm on Linux fails at spawn with an unhelpful message. TASK 6 MUST STATE THE LIMIT.
  (e) Nested error prefixes; successor.setup() runs on the desktop path; the pre-claim gate's
      refusal names a CLI remedy for a desktop launch; desktop_launch's `worktree` param actually
      receives the saved cwd; markdown_documents() walks the filesystem not the git index (and
      follows symlinks, and unwraps on unreadable dirs); test plumbing is pub crate API.
  (f) switch drops provider args on the desktop transport, and takes no pre-claim probe there.

=== STATUS: Tasks 1-5 of slice 4 complete at c3bc5b3. REMAINING: Task 6 (docs) + final
=== whole-branch review (9dfa3cc..HEAD) + Thomas's merge decision.
S4 Task 6: complete (commit c3bc5b3..6b56ee5, docs only, 4 files, README untouched at 175/180)
  - *** MY CLAIM WAS FALSE, caught by the implementer with an empirical test ***
    I carried forward from slice 3 that "attach tier makes the MCP claim tool reachable, because an
    attached session holds no lease". It does NOT. mcp_claim_value hardcodes from_provider=true,
    which requires real run credentials (HANDOVER_SESSION_ID / RUN_ID / CHECKPOINT_INBOX) that an
    attached session never has. Verified live: calling the MCP claim tool from an attached
    session's environment fails with "HANDOVER_SESSION_ID is required", NOT a lease refusal.
    What IS true and tested: the PLAIN CLI `handover arm` / `handover claim` (without
    --from-provider) become reachable on an attach-tier session because it holds no lease.
    The implementer documented the accurate mechanism rather than my unverifiable one.
    => The MCP `claim` tool remains effectively unreachable, same as slice 3 concluded. Its only
       route in is a provider process, which by construction holds a live lease. FOR FINAL REVIEW.
ALL 6 SLICE-4 TASKS COMPLETE at 6b56ee5 (11 commits from 9dfa3cc). Proceeding to final review.

FINAL WHOLE-BRANCH REVIEW (opus, 9dfa3cc..6b56ee5, 11 commits): "NOT READY"
  CRITICAL #1: a desktop claim journals NO binding for the target it opens. SessionAttached is
    emitted from exactly one place (attach_value), so after `codex app` opens, all three surfaces
    still report the PREDECESSOR: status.provider=claude, binding={supervised, claude, detached
    false}, list the same, doctor silent. The user is in codex's desktop app and Handover
    positively asserts claude is bound and supervised. Then the permanent part: the next arm from
    that session journals switch.requested {from: "claude", to: "claude"} and the rendered handover
    header reads "Provider: claude -> claude". Append-only + checksummed.
    Reproduced end to end with the capture seam. The attach-tier route to the SAME real-world state
    reports it completely differently ({attached, detached:true}, provider null) - two paths to
    "a desktop app is what you're using now", two irreconcilable reports.
    NOTE: ClaimedTransport::Desktop's OWN doc comment says "That is what makes a desktop session
    attach tier" - the code never does it. This state was unreachable before this slice.
    Fix A: append SessionAttached(provider=target) on a successful desktop launch. No new event
    kind, no new field. Costs re-acquiring SessionOperationLock after the launch (both call sites
    deliberately drop it first so the opened app can reach back over MCP).
    Fix B: accept and document, leaving the wrong `from` in the journal permanently.
  IMPORTANT #2: arm --replace appends switch.expired and prints "Superseded..." BEFORE the
    fallible preview_handover gate. Reproduced by corrupting a checkpoint blob: journal ends at
    switch.expired with no replacement, user told the supersede succeeded then told the command
    failed. Exactly the "half happened" state claim_core and next_launch_from_pending_arm both go
    out of their way to prevent. Fix: move the gate above the retire block.
  IMPORTANT #3: `handover switch codex -- --model gpt-5` silently DROPS the args on a desktop arm
    and exits 0 reporting success. The user need not know the arm was a desktop arm (a provider may
    have recorded it minutes earlier). Fix: eprintln naming the unapplied args.
  MINORS 4-10: "Opened..." printed on spawn() success which proves only exec, not that the app
    opened; 3 stacked error prefixes; previous_provider's error prints Tier's Debug name
    (capitalized) where everything else prints serde lowercase; list's `bound` vs `detached` are
    two unrelated meanings of one word; doctor states tier but never detachment (same asymmetry
    fixed for list.last_provider in ad22fb5); desktop_launch's `worktree` param takes a cwd and the
    docs repeat the wrong name; MCP `replace` description is narrower than the behaviour.
  VERIFIED CLEAN by construction, not reading: all 3 surfaces agree across supervised/attached/
    detached/no-binding; backward compat with slice 1-3 journals (max_by_key and the old
    .rev().find agree because appends are sequence-ordered); no new event kind or payload field
    anywhere on the branch; the ONE new append site (SwitchExpired) is under the lock; every reader
    of the two functions that now return None takes Option<Provider> already; the desktop failure
    advice IS true at print time (no arm remains, preview succeeds).
  Carried-minor triage: (f) promoted to Important #3 -> fix before merge. (a)(b)(c)(e-partial)
    after merge. (d) accept, documented. (g) accept for this slice, BUT the sharper gap is
    mcp_arm_value pinning from_provider=true too - so a desktop session cannot arm its way BACK
    over MCP, making the desktop leg a ONE-WAY TRIP a human must end from a terminal.

FINAL FIXES: complete (commits 6b56ee5..cc2b5c0, 7 logical commits)
  c34a087 Fix 1 (CRITICAL) - a desktop claim journals session.attached for its target.
    Reuses the EXISTING event; no new kind, no new field. Both call sites still drop the operation
    lock before launching (so the opened app can reach back over MCP) and RE-ACQUIRE it for the
    single append. A failed append warns and keeps the exit code - the app is already open, so
    failing would be useless; the warning names `handover attach <provider>` as the correction.
  a03862f Fix 2 - arm --replace renders BEFORE it retires (was: gate failure left no arm at all)
  9261910 Fix 3 - desktop switch names the provider args it dropped instead of exiting 0 silently
  575546f Fix 4 - MCP write tools DERIVE from_provider from HANDOVER_RUN_ID instead of pinning
    true. Grants no new privilege: with run credentials the strict path is byte-for-byte as
    before; without them it is exactly what the plain CLI already does for anyone in that worktree.
    Fixes the one-way desktop leg - a desktop session can now arm its way back.
  07969b2 Fix 5 - four wording/naming corrections
  f994d2a docs follow-on - session.attached now has two writers
  cc2b5c0 Fix round 2 - doctor no longer tells a desktop-hopped user they ran `handover attach`

RE-REVIEW OF THE FIX WAVE (opus, 6b56ee5..f994d2a): "Ready to merge: with fixes" (the one Minor
  became cc2b5c0). Reviewer verified BY CONSTRUCTION AND MUTATION, not by reading:
  - Fix 1's drop is load-bearing: keeping the lock across the launch HANGS (proved by mutation).
    No deadlock cycle - the app's MCP server is a separate process taking the same flock briefly.
    Constructed both claim paths, the failed-launch path (appends nothing, prior reporting intact),
    and a FORCED append failure (FIFO to pause between launch and append + chmod 0444 on
    events.jsonl) -> exit code preserved, correct warning.
  - Fix 4's security half proved by the reviewer's OWN mutation: forcing the derivation to false
    made ALL FOUR refusals vanish and let `claim` succeed, writing two switch.claimed events. So
    the derivation is load-bearing. Also verified partial credentials fail closed (RUN_ID set,
    SESSION_ID absent -> refused; EMPTY RUN_ID counts as present -> strict path).
  - Confirmed no privilege granted: the worktree path sets armed_run: None, so an MCP-armed switch
    can never become the unprompted dead-lease recovery an in-run arm can.
  - Known and accepted: launch and append are not atomic. The reviewer widened the window
    artificially and reproduced the old symptom; in the real path it is the microseconds between
    spawn() returning and one append, which no app can cold-start inside.
  - Replaced test judged strictly stronger: 1 test asserting "no run env -> refuse" (the premise
    Fix 4 deliberately changes) became 4 driving genuine run state, asserting error TEXT.
=== SLICE 4 COMPLETE at cc2b5c0. 18 commits from 9dfa3cc. 340 passed / 0 failed, fmt + clippy
=== clean. Awaiting Thomas's merge decision.

=== SLICE 4 MERGED (Thomas, 2026-08-05) ===
Fast-forward: main 9dfa3cc -> cc2b5c0 (18 commits, 17 files, +2707/-133).
New files: src/launch.rs, src/session.rs, tests/attach_tier.rs, tests/desktop_transport.rs.
Pushed to origin/main, 0 unpushed. CI run 31003061883 watching.
This completes ALL FOUR SLICES of .superpowers/specs/2026-07-28-handover-command-design.md:
  1 plumbing (PR #14) / 2 porcelain (PR #17) / 3 experience (9dfa3cc) / 4 desktop+tier (cc2b5c0).

FOLLOW-UPS CARRIED OUT OF THE FEATURE (none block anything, all recorded above in detail):
  - arm_command: 8 args, 3 consecutive bools, #[allow(clippy::too_many_arguments)]. A slice-3
    reviewer predicted it and asked for a struct; my plan mandated the positional signature.
  - check_sessions deep-clones every event of every session on every doctor run for one boolean.
    Fix is an iterator signature on binding().
  - markdown_documents() in tests/repository_contract.rs walks the FILESYSTEM not the git index,
    follows symlinks, and unwraps on unreadable dirs. Matches `git ls-files '*.md'` in a clean
    checkout, so CI is safe; the exposure is a dev tree with a nested worktree.
  - Test plumbing (TEST_LAUNCH_LOG_ENV, CaptureLauncher, EnvironmentLauncher) is pub API on a
    published crate.
  - The desktop launch and its session.attached append are not atomic (microseconds; no app can
    cold-start inside the window). Documented, accepted.
  - SpawnLauncher's real spawn path is executed by no test - the seam cannot cross that line.
CI on merged main cc2b5c0: run 31003061883 ALL 4 JOBS SUCCESS
  (test + install.sh on both ubuntu-latest and macos-latest).
  The ubuntu leg was the one at risk: src/launch.rs hardcodes `open` for the Claude desktop
  transport, which does not exist on Linux. The tests assert the launch SPEC rather than executing
  it, so it holds - now confirmed rather than assumed.
=== FEATURE COMPLETE. All 4 slices merged and green on both platforms. ===
