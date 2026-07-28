use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Error, Result};
use crate::model::{Event, EventKind, Provider, RunId, Surface};
use crate::runtime::Runtime;
use crate::store::SessionStore;

pub const DEFAULT_TTL: &str = "15m";

const MAX_TTL_SECONDS: u64 = 24 * 60 * 60;

pub fn parse_ttl(value: &str) -> Result<std::time::Duration> {
    let trimmed = value.trim();
    let (digits, multiplier) = if let Some(head) = trimmed.strip_suffix('s') {
        (head, 1u64)
    } else if let Some(head) = trimmed.strip_suffix('m') {
        (head, 60)
    } else if let Some(head) = trimmed.strip_suffix('h') {
        (head, 3_600)
    } else {
        return Err(Error::InvalidState(format!(
            "ttl {value:?} must end in s, m, or h"
        )));
    };
    let amount: u64 = digits.parse().map_err(|_| {
        Error::InvalidState(format!("ttl {value:?} must be a whole number of units"))
    })?;
    let seconds = amount
        .checked_mul(multiplier)
        .ok_or_else(|| Error::InvalidState(format!("ttl {value:?} overflows")))?;
    if seconds == 0 || seconds > MAX_TTL_SECONDS {
        return Err(Error::InvalidState(format!(
            "ttl {value:?} must be between 1s and 24h"
        )));
    }
    Ok(std::time::Duration::from_secs(seconds))
}

pub fn expires_at(runtime: &dyn Runtime, ttl: std::time::Duration) -> Result<String> {
    (parse_clock(&runtime.now()?)? + ttl)
        .format(&Rfc3339)
        .map_err(|error| Error::InvalidState(format!("cannot format arm expiry: {error}")))
}

fn parse_clock(value: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|error| Error::InvalidState(format!("{value:?} is not RFC 3339: {error}")))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingArm {
    pub sequence: u64,
    pub to: Provider,
    pub surface: Surface,
    pub expires_at: String,
    pub armed_run: Option<RunId>,
}

/// The newest `switch.armed` with no matching `switch.claimed` or
/// `switch.expired`. Pure — expiry is not considered here.
pub(crate) fn latest_unresolved(events: &[Event]) -> Option<PendingArm> {
    let mut candidate: Option<PendingArm> = None;
    for event in events {
        match &event.kind {
            EventKind::SwitchArmed {
                to,
                surface,
                expires_at,
            } => {
                candidate = Some(PendingArm {
                    sequence: event.sequence,
                    to: *to,
                    surface: *surface,
                    expires_at: expires_at.clone(),
                    armed_run: event.run_id.clone(),
                });
            }
            EventKind::SwitchClaimed { armed_sequence, .. }
            | EventKind::SwitchExpired { armed_sequence } => {
                if candidate
                    .as_ref()
                    .is_some_and(|arm| arm.sequence == *armed_sequence)
                {
                    candidate = None;
                }
            }
            _ => {}
        }
    }
    candidate
}

/// The pending arm for this session, or `None`.
///
/// Expiry is evaluated lazily: nothing runs in the background to notice a
/// deadline pass, so an arm found past its TTL is retired here — the
/// `switch.expired` event is appended before this returns `None`. Callers must
/// hold `SessionOperationLock`, because this can write.
pub fn pending(
    store: &SessionStore,
    runtime: &dyn Runtime,
    events: &[Event],
) -> Result<Option<PendingArm>> {
    let Some(arm) = latest_unresolved(events) else {
        return Ok(None);
    };
    if parse_clock(&runtime.now()?)? < parse_clock(&arm.expires_at)? {
        return Ok(Some(arm));
    }
    store.append(
        runtime,
        None,
        None,
        EventKind::SwitchExpired {
            armed_sequence: arm.sequence,
        },
    )?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use tempfile::TempDir;

    use super::{DEFAULT_TTL, expires_at, parse_ttl, pending};
    use crate::arm::latest_unresolved;
    use crate::error::Result;
    use crate::model::{
        Event, EventKind, GitSnapshot, OperationId, Provider, RunId, SessionId, Surface,
        WorktreeIdentity,
    };
    use crate::runtime::Runtime;
    use crate::store::{SessionStore, StateLayout};

    struct FixedClock(Mutex<&'static str>);

    impl FixedClock {
        fn new(now: &'static str) -> Self {
            Self(Mutex::new(now))
        }

        fn set(&self, now: &'static str) {
            *self.0.lock().unwrap() = now;
        }
    }

    impl Runtime for FixedClock {
        fn now(&self) -> Result<String> {
            Ok((*self.0.lock().unwrap()).to_owned())
        }
        fn session_id(&self) -> SessionId {
            SessionId::new()
        }
        fn run_id(&self) -> RunId {
            RunId::new()
        }
        fn operation_id(&self) -> OperationId {
            OperationId::new()
        }
    }

    #[test]
    fn ttl_accepts_second_minute_and_hour_suffixes() {
        assert_eq!(parse_ttl("30s").unwrap().as_secs(), 30);
        assert_eq!(parse_ttl("15m").unwrap().as_secs(), 900);
        assert_eq!(parse_ttl("2h").unwrap().as_secs(), 7200);
        assert_eq!(parse_ttl(DEFAULT_TTL).unwrap().as_secs(), 900);
    }

    #[test]
    fn ttl_refuses_unbounded_zero_and_unsuffixed_values() {
        for bad in ["0m", "15", "m", "-5m", "abc", "25h", ""] {
            assert!(parse_ttl(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn expiry_is_the_clock_plus_the_ttl_in_rfc3339() {
        let clock = FixedClock::new("2026-07-28T10:00:00Z");
        assert_eq!(
            expires_at(&clock, parse_ttl("15m").unwrap()).unwrap(),
            "2026-07-28T10:15:00Z"
        );
    }

    fn armed(sequence: u64, expires_at: &str) -> Event {
        Event {
            schema_version: 1,
            sequence,
            occurred_at: "2026-07-28T10:00:00Z".into(),
            recorded_at: "2026-07-28T10:00:00Z".into(),
            session_id: SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
            run_id: None,
            provider: None,
            idempotency_key: None,
            kind: EventKind::SwitchArmed {
                to: Provider::Codex,
                surface: Surface::Auto,
                expires_at: expires_at.into(),
            },
        }
    }

    fn resolving(sequence: u64, kind: EventKind) -> Event {
        let mut event = armed(sequence, "2026-07-28T10:15:00Z");
        event.kind = kind;
        event
    }

    #[test]
    fn the_newest_unresolved_arm_wins() {
        let events = [
            armed(4, "2026-07-28T10:15:00Z"),
            armed(9, "2026-07-28T10:20:00Z"),
        ];
        assert_eq!(latest_unresolved(&events).unwrap().sequence, 9);
    }

    #[test]
    fn a_claimed_or_expired_arm_is_resolved() {
        for kind in [
            EventKind::SwitchClaimed {
                armed_sequence: 4,
                to: Provider::Codex,
            },
            EventKind::SwitchExpired { armed_sequence: 4 },
        ] {
            let events = [armed(4, "2026-07-28T10:15:00Z"), resolving(5, kind)];
            assert!(latest_unresolved(&events).is_none());
        }
    }

    #[test]
    fn resolving_a_different_arm_leaves_the_pending_one_alone() {
        let events = [
            armed(4, "2026-07-28T10:15:00Z"),
            resolving(5, EventKind::SwitchExpired { armed_sequence: 2 }),
        ];
        assert_eq!(latest_unresolved(&events).unwrap().sequence, 4);
    }

    fn snapshot() -> GitSnapshot {
        let common_git_dir = PathBuf::from("/repo/.git");
        let git_dir = PathBuf::from("/repo/.git/worktrees/oauth");
        GitSnapshot {
            identity: WorktreeIdentity {
                key: WorktreeIdentity::derive_key(&common_git_dir, &git_dir),
                common_git_dir,
                git_dir,
                worktree: PathBuf::from("/work/oauth"),
                cwd_relative: PathBuf::from("apps/web"),
            },
            branch: Some("feat/oauth".into()),
            head: "deadbeef".into(),
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: Vec::new(),
            dirty_submodules: Vec::new(),
        }
    }

    /// A real `SessionStore` backed by a temp directory, with a single
    /// `switch.armed` event already appended. Returns the store and the
    /// sequence number of that arm.
    fn store_with_armed_session(
        temp: &TempDir,
        runtime: &FixedClock,
        expires_at: &str,
    ) -> (SessionStore, u64) {
        let layout = StateLayout::new(temp.path().join("state"));
        let store = SessionStore::create(&layout, runtime, snapshot()).unwrap();
        let event = store
            .append(
                runtime,
                None,
                None,
                EventKind::SwitchArmed {
                    to: Provider::Codex,
                    surface: Surface::Auto,
                    expires_at: expires_at.into(),
                },
            )
            .unwrap();
        (store, event.sequence)
    }

    #[test]
    fn pending_returns_the_arm_before_it_expires() {
        let temp = TempDir::new().unwrap();
        let runtime = FixedClock::new("2026-07-28T10:00:00Z");
        let (store, sequence) = store_with_armed_session(&temp, &runtime, "2026-07-28T10:15:00Z");

        runtime.set("2026-07-28T10:14:59Z");
        let events_before = store.events().unwrap();
        let arm = pending(&store, &runtime, &events_before)
            .unwrap()
            .expect("arm should still be pending one second before expiry");

        assert_eq!(arm.sequence, sequence);
        assert_eq!(arm.to, Provider::Codex);
        assert_eq!(arm.surface, Surface::Auto);
        assert_eq!(arm.expires_at, "2026-07-28T10:15:00Z");

        // No expiry should have been journaled.
        let events_after = store.events().unwrap();
        assert_eq!(events_after.len(), events_before.len());
    }

    #[test]
    fn pending_retires_the_arm_when_the_clock_reaches_or_passes_expiry() {
        for now in ["2026-07-28T10:15:00Z", "2026-07-28T10:20:00Z"] {
            let temp = TempDir::new().unwrap();
            let runtime = FixedClock::new("2026-07-28T10:00:00Z");
            let (store, sequence) =
                store_with_armed_session(&temp, &runtime, "2026-07-28T10:15:00Z");

            runtime.set(now);
            let events_before = store.events().unwrap();
            let result = pending(&store, &runtime, &events_before).unwrap();
            assert!(result.is_none(), "now={now} should have expired the arm");

            let events_after = store.events().unwrap();
            assert_eq!(
                events_after.len(),
                events_before.len() + 1,
                "now={now} should have journaled exactly one switch.expired event"
            );
            assert_eq!(
                events_after.last().unwrap().kind,
                EventKind::SwitchExpired {
                    armed_sequence: sequence
                },
                "now={now}"
            );
        }
    }
}
