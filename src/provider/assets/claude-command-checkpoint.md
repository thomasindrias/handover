---
description: Record a Handover narrative checkpoint for this session
---

Write a Handover narrative checkpoint describing the work in this session.

Handover's hooks already record what happened — files touched, commands run,
their failures. They cannot record why. This checkpoint is the part only you
can supply, and it is what the next provider reads when the session is handed
over.

Base it on the actual work in this conversation, not on a template. If a fact
is not established, leave its array empty rather than inventing one.

- `objective` — what this session is trying to achieve, in one line.
- `summary` — where the work actually stands right now, including what is
  unfinished or uncertain.
- `decisions` — choices made and the reason, as `{"statement": "...",
  "reason": "..."}`. Reasons matter more than the choices.
- `assumptions` — what was taken as true without verifying.
- `constraints` — what the next session must not break.
- `completed`, `in_progress`, `blockers` — plain statements.
- `next_steps` — the exact next actions, most immediate first.
- `related_event_sequences` — sequence numbers from `handover log --json`
  that support the above, sorted and unique. Leave empty if unsure.

`objective`, `summary`, and at least one `next_steps` entry are required. Every
other array may be empty.

Then submit it by piping the JSON into the command below. The
`--from-provider` flag is required: an attached provider may not use the human
mutation path, and without the flag Handover refuses the command.

```sh
printf '%s' '<the JSON>' | handover checkpoint --format json --from-provider
```

Report whether Handover accepted it. If it rejected the payload, show the error
and correct the JSON rather than retrying unchanged.
