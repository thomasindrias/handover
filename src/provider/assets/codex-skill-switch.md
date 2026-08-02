---
name: handover-switch
description: Hand this Handover session over to another coding provider. Use when the user asks to switch to Claude or Codex, to hand off, or to continue this work in a different provider.
---

# Hand this session over to another provider

Handover is already tracking this session. Arming a switch records the intent
now; it completes when this session exits, and the target provider comes up in
the same terminal already holding the handover — nothing is duplicated and
nothing is reconstructed.

Take the target from the user's request. It must be exactly `claude` or
`codex`. If they did not name one, ask. Do not guess, and do not arm a provider
they did not name.

## 1. Write a narrative checkpoint first

The handover carries the narrative. Handover's hooks already recorded what
happened — files touched, commands run, their failures — but not why. Without a
fresh checkpoint the next provider receives a thin document.

Build the JSON from the actual work in this conversation. Every key below must
be present. `objective`, `summary`, and at least one `next_steps` entry are
required to carry content; every other array may stay empty rather than
invented. Fill in this exact shape and submit it:

```sh
printf '%s' '{"objective":"...","summary":"...","decisions":[],"assumptions":[],"constraints":[],"completed":[],"in_progress":[],"blockers":[],"next_steps":["..."],"related_event_sequences":[]}' \
  | "$HANDOVER_HOOK_BIN" checkpoint --format json --from-provider
```

A `decisions` entry is `{"statement": "...", "reason": "..."}`.
`related_event_sequences` holds sequence numbers from `handover log --json`,
sorted and unique; leave it empty if unsure.

Report whether Handover accepted it. If it rejected the payload, show the error
and correct the JSON rather than retrying unchanged.

## 2. Arm the switch

```sh
"$HANDOVER_HOOK_BIN" arm "<provider>" --from-provider
```

`<provider>` is the target the user named. The `--from-provider` flag is
required: it is the only path Handover accepts from a provider it launched.

## 3. Report, then stop

If Handover refused, show its exact message and stop. Do not retry unchanged,
do not edit Handover's state directly, and do not look for another route — the
refusal is the same one `handover switch` would give, and it is telling you the
session is not ready to move.

If it accepted, tell the user the switch is armed and that quitting this
session hands over to that provider in the same terminal. Then stop. Quitting
is theirs to do, not yours.
