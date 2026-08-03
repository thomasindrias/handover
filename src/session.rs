//! What is bound to a session right now, derived from its events.
//!
//! Three surfaces report this — `status`, `list`, and `doctor` — and they must
//! not be able to disagree, so the derivation lives here rather than three
//! times over.

use serde::Serialize;

use crate::model::{Event, EventKind, Provider};

/// How a session is bound: to a provider Handover launched and supervises, or
/// to one it merely adopted.
///
/// An attached session has no lifecycle hooks, so its journal holds narrative
/// checkpoints and refreshed Git facts but no observed activity. Reporting the
/// tier is how Handover avoids implying a completeness the session does not
/// have.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Supervised,
    Attached,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Binding {
    pub tier: Tier,
    pub provider: Option<Provider>,
    pub sequence: u64,
    /// The attachment is still on screen but no longer recorded: a claim has
    /// since moved the session on, and Handover cannot make a desktop app quit.
    pub detached: bool,
}

/// The session's current binding, or `None` if nothing was ever bound.
///
/// The binding is whichever of `run.started` and `session.attached` came last,
/// so a worktree can move between tiers in either direction and still be
/// reported honestly.
pub fn binding(events: &[Event]) -> Option<Binding> {
    let latest = events.iter().rev().find(|event| {
        matches!(
            event.kind,
            EventKind::RunStarted { .. } | EventKind::SessionAttached {}
        )
    })?;
    let tier = match latest.kind {
        EventKind::SessionAttached {} => Tier::Attached,
        _ => Tier::Supervised,
    };
    // Only an attachment can be left detached. A supervised run's exit is
    // already journaled as `run.stopped`, and its lease is cleared.
    let detached = tier == Tier::Attached
        && events.iter().any(|event| {
            event.sequence > latest.sequence
                && matches!(event.kind, EventKind::SwitchClaimed { .. })
        });
    Some(Binding {
        tier,
        provider: latest.provider,
        sequence: latest.sequence,
        detached,
    })
}

#[cfg(test)]
mod tests {
    use super::{Tier, binding};
    use crate::model::{Event, EventKind, Provider, SessionId};

    /// Events carry only what this derivation reads; the rest are filled with
    /// placeholders that satisfy `Event`'s actual field list.
    fn event(sequence: u64, provider: Option<Provider>, kind: EventKind) -> Event {
        Event {
            schema_version: 1,
            sequence,
            occurred_at: "2026-08-03T00:00:00Z".into(),
            recorded_at: "2026-08-03T00:00:00Z".into(),
            session_id: SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
            run_id: None,
            provider,
            idempotency_key: None,
            kind,
        }
    }

    fn run_started(sequence: u64, provider: Provider) -> Event {
        event(
            sequence,
            Some(provider),
            EventKind::RunStarted {
                cwd: "/w".into(),
                args: Vec::new(),
                supervisor_pid: 1,
            },
        )
    }

    fn attached(sequence: u64, provider: Provider) -> Event {
        event(sequence, Some(provider), EventKind::SessionAttached {})
    }

    fn claimed(sequence: u64) -> Event {
        event(
            sequence,
            None,
            EventKind::SwitchClaimed {
                armed_sequence: 1,
                to: Provider::Codex,
                transition_checkpoint_sequence: 1,
            },
        )
    }

    #[test]
    fn a_session_with_no_binding_events_has_no_binding() {
        assert_eq!(binding(&[]), None);
    }

    #[test]
    fn the_most_recent_binding_event_wins_in_both_directions() {
        let adopted_then_run = [
            attached(1, Provider::Claude),
            run_started(2, Provider::Codex),
        ];
        let bound = binding(&adopted_then_run).expect("a binding exists");
        assert_eq!(bound.tier, Tier::Supervised);
        assert_eq!(bound.provider, Some(Provider::Codex));

        let run_then_adopted = [
            run_started(1, Provider::Codex),
            attached(2, Provider::Claude),
        ];
        let bound = binding(&run_then_adopted).expect("a binding exists");
        assert_eq!(bound.tier, Tier::Attached);
        assert_eq!(bound.provider, Some(Provider::Claude));
    }

    #[test]
    fn an_attachment_a_claim_followed_is_reported_detached() {
        // The old desktop window is still on screen; Handover cannot quit it,
        // and nothing it does from here is journaled.
        let events = [attached(1, Provider::Claude), claimed(2)];
        assert!(binding(&events).expect("a binding exists").detached);
    }

    #[test]
    fn a_claim_before_the_attachment_does_not_detach_it() {
        let events = [claimed(1), attached(2, Provider::Claude)];
        assert!(!binding(&events).expect("a binding exists").detached);
    }

    #[test]
    fn a_supervised_binding_is_never_reported_detached() {
        // A run's exit is journaled as `run.stopped` and clears its lease, so
        // "detached" would be a second, weaker way of saying the same thing.
        let events = [run_started(1, Provider::Codex), claimed(2)];
        assert!(!binding(&events).expect("a binding exists").detached);
    }
}
