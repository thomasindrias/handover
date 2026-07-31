---
description: Hand this Handover session over to another provider
argument-hint: claude | codex
---

Arm a Handover switch to the provider named in $ARGUMENTS. This session keeps
running. The switch completes when it exits, and the target comes up in the
same terminal already holding the handover — nothing is duplicated and nothing
is reconstructed.

If $ARGUMENTS is empty, or is not exactly `claude` or `codex`, stop and ask
which provider to hand over to. Do not guess, and do not arm a provider the
user did not name.

## 1. Write a narrative checkpoint first

The handover carries the narrative. Handover's hooks already recorded what
happened — files touched, commands run, their failures — but not why. Without a
fresh checkpoint the next provider receives a thin document.

Build the JSON from the actual work in this conversation, exactly as
`/handover-checkpoint` describes: `objective`, `summary`, and at least one
`next_steps` entry are required, and every other array may be empty rather than
invented. Then submit it:

```sh
printf '%s' '<the JSON>' | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
```

## 2. Arm the switch

```sh
"$HANDOVER_HOOK_BIN" arm <provider> --from-provider
```

`<provider>` is the value from $ARGUMENTS. The `--from-provider` flag is
required: it is the only path Handover accepts from a provider it launched.

## 3. Report, then stop

If Handover refused, show its exact message and stop. Do not retry unchanged,
do not edit Handover's state directly, and do not look for another route — the
refusal is the same one `handover switch` would give, and it is telling you the
session is not ready to move.

If it accepted, tell the user the switch is armed and that quitting this
session hands over to that provider in the same terminal. Then stop. Quitting
is theirs to do, not yours.
