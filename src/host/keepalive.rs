//! Pure keep-alive state machine for the host engine (issue #35).
//!
//! `host_group` (in `engine.rs`) used to inline every keep-alive policy decision
//! — reconnect on relay drop, exponential backoff, periodic token re-mint, and
//! the auth-error → relogin path — inside an async loop fused to the SDK's
//! `RelayTunnelHost`, leaving the most failure-prone logic in the app untested.
//!
//! This module holds that policy as a pure transition function with **zero**
//! SDK, CLI, or channel dependencies: it imports only [`std::time::Duration`].
//! The driver feeds it [`ConnEvent`]s (connection outcomes) and executes the
//! returned [`Action`]s. Because it is pure, it is unit-tested without the
//! vendored-OpenSSL toolchain — the tests run under a plain `cargo test`.

use std::time::Duration;

/// Re-mint the host/manage tokens before their ~24h expiry. 20h leaves headroom.
pub const REMINT_AFTER: Duration = Duration::from_secs(20 * 60 * 60);
/// Base backoff after a relay drop; doubles up to [`RECONNECT_BACKOFF_MAX`].
const RECONNECT_BACKOFF_START: Duration = Duration::from_secs(2);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Consecutive public-probe `Down` cycles, on a still-`Hosting` group, that force
/// a watchdog reconnect (issue #39). Requiring a streak — not a single `Down` —
/// rides out a one-off probe blip; at the Health probe's cadence this is a few
/// seconds of a confirmed-dead public URL before the engine tears the (apparently
/// live but zombie) relay session down and reconnects.
pub const PROBE_DOWN_THRESHOLD: u32 = 3;

/// Why a connect attempt failed — drives whether the driver retries, stops, or
/// asks the user to re-authenticate. The driver classifies the raw error string
/// (via the `devtunnel` helpers) into one of these so the state machine stays
/// pure and free of string parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnFailure {
    /// Expired or absent CLI sign-in: retrying is pointless until the user
    /// re-authenticates.
    Auth,
    /// Non-recoverable (e.g. a `400` from the management API rejecting the
    /// request): retrying with the same inputs can never succeed, so stop.
    Fatal,
    /// Recoverable (network/relay hiccup): back off and retry.
    Transient,
}

/// Outcome of a public-URL health probe, fed into the watchdog (issue #39). The
/// kinds mirror the Health probe's own distinction and exist so the false-positive
/// guard lives in the pure policy: only [`ProbeOutcome::Down`] (relay unreachable)
/// can drive a reconnect — [`ProbeOutcome::ServiceDown`] (relay answered 5xx, so
/// it is alive but the local upstream is down, e.g. a server restart) never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The public URL served a healthy response; the tunnel is up.
    Healthy,
    /// The relay was unreachable (network error/timeout) — a possible zombie
    /// tunnel. A streak of these on a `Hosting` group forces a reconnect.
    Down,
    /// The relay answered but with a 5xx: the relay is alive, the local upstream
    /// is down. Never a reconnect trigger — reconnecting would not revive a
    /// restarting local server and would churn a perfectly good relay session.
    ServiceDown,
}

/// A connection outcome fed into the state machine by the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnEvent {
    /// The relay connect (and port forwarding) succeeded.
    Connected,
    /// The live relay session dropped; the driver wants to reconnect.
    RelayDropped,
    /// The ~20h re-mint timer fired; reconnect with fresh tokens.
    RemintDue,
    /// A connect attempt failed, carrying why (see [`ConnFailure`]).
    ConnectFailed(ConnFailure),
    /// A public-URL health probe reported the given outcome (issue #39). Only a
    /// streak of [`ProbeOutcome::Down`] on a still-`Hosting` group yields
    /// [`Action::Reconnect`]; every other outcome (and any probe before the first
    /// successful connect) is absorbed as [`Action::Await`].
    Probe(ProbeOutcome),
}

/// What the driver should execute next, returned by [`KeepAliveState::next`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Hold the live connection and wait for the next outcome (no sleep).
    Await,
    /// Sleep for the given backoff, then (re)connect.
    Sleep(Duration),
    /// The sign-in is expired: emit `ReloginRequired`, surface an error, stop.
    Relogin,
    /// A non-recoverable error: surface it and stop. No retry, no relogin prompt.
    Fail,
    /// The public-URL watchdog (issue #39) judged the live session a zombie: a
    /// `Down` streak reached [`PROBE_DOWN_THRESHOLD`] while still `Hosting`. The
    /// driver force-drops the (apparently live) relay handle and reconnects now —
    /// no extra sleep, funnelling into the same `connect_once` path so any ensuing
    /// failure backs off normally (no parallel reconnect logic).
    Reconnect,
}

/// Presentation phase. The driver maps it to `HostState::Connecting` (first
/// attempt) vs. `HostState::Reconnecting` (every attempt after the first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// No successful connect or failed attempt yet — show "Connecting".
    Initial,
    /// At least one attempt has happened — show "Reconnecting".
    Reconnect,
}

/// The keep-alive policy state: the current reconnect backoff and whether this
/// is still the first connection attempt. Pure — no SDK/CLI/channel state.
pub struct KeepAliveState {
    backoff: Duration,
    first_attempt: bool,
    /// Whether a live relay session is currently believed up (between a
    /// [`ConnEvent::Connected`] and the next session-ending event). The watchdog
    /// only counts probe `Down`s while this holds — a probe failing during a
    /// connect attempt is not a zombie, just the connect not landed yet.
    connected: bool,
    /// Consecutive [`ProbeOutcome::Down`] cycles seen while `connected`. Any other
    /// probe outcome, or a session-ending event, resets it to zero.
    down_streak: u32,
}

impl KeepAliveState {
    /// A fresh state: backoff at [`RECONNECT_BACKOFF_START`], first attempt.
    pub fn new() -> Self {
        Self {
            backoff: RECONNECT_BACKOFF_START,
            first_attempt: true,
            connected: false,
            down_streak: 0,
        }
    }

    /// Whether no attempt has completed yet (drives Connecting vs Reconnecting).
    pub fn first_attempt(&self) -> bool {
        self.first_attempt
    }

    /// The presentation phase for the next attempt.
    pub fn phase(&self) -> Phase {
        if self.first_attempt {
            Phase::Initial
        } else {
            Phase::Reconnect
        }
    }

    /// Advances the state machine for one connection outcome and returns the
    /// action the driver must execute. Mirrors the original `host_group`
    /// control flow exactly (asymmetric backoff reset: a success resets the
    /// backoff, consecutive connect-failures keep doubling it).
    pub fn next(&mut self, event: ConnEvent) -> Action {
        match event {
            // Success: reset the backoff and leave the first-attempt phase. A live
            // session is now up, so the watchdog starts counting from a clean slate.
            ConnEvent::Connected => {
                self.backoff = RECONNECT_BACKOFF_START;
                self.first_attempt = false;
                self.connected = true;
                self.down_streak = 0;
                Action::Await
            }
            // A live session ended (drop or re-mint): sleep the current backoff,
            // then double it (capped) for the next attempt. The session is no
            // longer up, so the watchdog stops counting until the next connect.
            ConnEvent::RelayDropped | ConnEvent::RemintDue => {
                self.end_session();
                Action::Sleep(self.bump())
            }
            // Expired sign-in: stop and ask the user to re-authenticate.
            ConnEvent::ConnectFailed(ConnFailure::Auth) => {
                self.end_session();
                Action::Relogin
            }
            // Non-recoverable error: stop. Retrying identical inputs would loop
            // forever (re-minting tokens each cycle) without ever succeeding.
            ConnEvent::ConnectFailed(ConnFailure::Fatal) => {
                self.first_attempt = false;
                self.end_session();
                Action::Fail
            }
            // Recoverable connect failure: leave the first-attempt phase and
            // back off without resetting (consecutive failures keep doubling).
            ConnEvent::ConnectFailed(ConnFailure::Transient) => {
                self.first_attempt = false;
                self.end_session();
                Action::Sleep(self.bump())
            }
            // Public-URL watchdog (issue #39). Only counts while a live session is
            // up; outside one (during a connect attempt) a failing probe is just
            // the connect not landed yet, not a zombie.
            ConnEvent::Probe(outcome) => self.on_probe(outcome),
        }
    }

    /// Applies a health-probe outcome to the watchdog and returns the action.
    ///
    /// A streak of [`ProbeOutcome::Down`] reaching [`PROBE_DOWN_THRESHOLD`] on a
    /// live session yields [`Action::Reconnect`] (and resets the streak so the next
    /// trigger needs a fresh full streak — no tight reconnect loop). Every other
    /// outcome resets the streak; [`ProbeOutcome::ServiceDown`] in particular never
    /// triggers a reconnect (relay alive, local upstream down). A probe arriving
    /// while no session is up is absorbed as [`Action::Await`].
    fn on_probe(&mut self, outcome: ProbeOutcome) -> Action {
        if !self.connected || outcome != ProbeOutcome::Down {
            self.down_streak = 0;
            return Action::Await;
        }
        self.down_streak += 1;
        if self.down_streak >= PROBE_DOWN_THRESHOLD {
            self.end_session();
            Action::Reconnect
        } else {
            Action::Await
        }
    }

    /// Marks the live session as ended: clears the connected flag and the watchdog
    /// streak. Called for every event that tears down or abandons the session.
    fn end_session(&mut self) {
        self.connected = false;
        self.down_streak = 0;
    }

    /// Returns the current backoff and then doubles it, capped at
    /// [`RECONNECT_BACKOFF_MAX`].
    fn bump(&mut self) -> Duration {
        let current = self.backoff;
        self.backoff = (self.backoff * 2).min(RECONNECT_BACKOFF_MAX);
        current
    }
}

impl Default for KeepAliveState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// Extracts the sleep duration from an [`Action::Sleep`]; panics otherwise so
    /// a wrong action is an obvious test failure rather than a silent skip.
    fn sleep_of(action: Action) -> Duration {
        match action {
            Action::Sleep(d) => d,
            other => panic!("expected Action::Sleep, got {other:?}"),
        }
    }

    #[test]
    fn backoff_progression_on_repeated_connect_failures() {
        let mut state = KeepAliveState::new();
        let expected = [2u64, 4, 8, 16, 32, 60, 60];
        let got: Vec<u64> = (0..expected.len())
            .map(|_| {
                sleep_of(state.next(ConnEvent::ConnectFailed(ConnFailure::Transient))).as_secs()
            })
            .collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn success_resets_backoff_before_next_drop() {
        let mut state = KeepAliveState::new();
        // Grow the backoff with two recoverable failures (2s, then 4s).
        assert_eq!(
            sleep_of(state.next(ConnEvent::ConnectFailed(ConnFailure::Transient))),
            secs(2)
        );
        assert_eq!(
            sleep_of(state.next(ConnEvent::ConnectFailed(ConnFailure::Transient))),
            secs(4)
        );
        // A successful connect returns Await and resets the backoff.
        assert_eq!(state.next(ConnEvent::Connected), Action::Await);
        // The reconnect sleep after the next relay drop is back to the start.
        assert_eq!(sleep_of(state.next(ConnEvent::RelayDropped)), secs(2));
    }

    #[test]
    fn remint_after_success_sleeps_start_and_remint_const_is_20h() {
        let mut state = KeepAliveState::new();
        assert_eq!(state.next(ConnEvent::Connected), Action::Await);
        assert_eq!(sleep_of(state.next(ConnEvent::RemintDue)), secs(2));
        assert_eq!(REMINT_AFTER, Duration::from_secs(72_000));
    }

    #[test]
    fn auth_error_yields_relogin() {
        let mut state = KeepAliveState::new();
        assert_eq!(
            state.next(ConnEvent::ConnectFailed(ConnFailure::Auth)),
            Action::Relogin
        );
    }

    #[test]
    fn fatal_error_yields_fail_and_does_not_retry() {
        let mut state = KeepAliveState::new();
        // A non-recoverable failure stops the task instead of backing off.
        assert_eq!(
            state.next(ConnEvent::ConnectFailed(ConnFailure::Fatal)),
            Action::Fail
        );
        // It also leaves the first-attempt phase, like any completed attempt.
        assert!(!state.first_attempt());
    }

    /// Drives the state machine to a live `Hosting` session, the precondition for
    /// every watchdog test below.
    fn connected_state() -> KeepAliveState {
        let mut state = KeepAliveState::new();
        assert_eq!(state.next(ConnEvent::Connected), Action::Await);
        state
    }

    #[test]
    fn probe_down_streak_reaching_threshold_triggers_reconnect() {
        let mut state = connected_state();
        // The first PROBE_DOWN_THRESHOLD-1 downs are absorbed while the streak grows.
        for _ in 0..PROBE_DOWN_THRESHOLD - 1 {
            assert_eq!(
                state.next(ConnEvent::Probe(ProbeOutcome::Down)),
                Action::Await
            );
        }
        // The threshold-th consecutive down forces the watchdog reconnect.
        assert_eq!(
            state.next(ConnEvent::Probe(ProbeOutcome::Down)),
            Action::Reconnect
        );
    }

    #[test]
    fn service_down_never_triggers_reconnect() {
        let mut state = connected_state();
        // Far past the threshold: a relay-alive/upstream-down result must never
        // reconnect (it would churn a good relay and not revive a restarting server).
        for _ in 0..PROBE_DOWN_THRESHOLD * 3 {
            assert_eq!(
                state.next(ConnEvent::Probe(ProbeOutcome::ServiceDown)),
                Action::Await
            );
        }
    }

    #[test]
    fn healthy_probe_resets_the_down_streak() {
        let mut state = connected_state();
        // Build the streak to one below the threshold, then a healthy probe clears it.
        for _ in 0..PROBE_DOWN_THRESHOLD - 1 {
            assert_eq!(
                state.next(ConnEvent::Probe(ProbeOutcome::Down)),
                Action::Await
            );
        }
        assert_eq!(
            state.next(ConnEvent::Probe(ProbeOutcome::Healthy)),
            Action::Await
        );
        // A fresh full streak is now required — the next down does not trigger.
        assert_eq!(
            state.next(ConnEvent::Probe(ProbeOutcome::Down)),
            Action::Await
        );
    }

    #[test]
    fn service_down_in_the_middle_resets_the_down_streak() {
        let mut state = connected_state();
        assert_eq!(
            state.next(ConnEvent::Probe(ProbeOutcome::Down)),
            Action::Await
        );
        assert_eq!(
            state.next(ConnEvent::Probe(ProbeOutcome::Down)),
            Action::Await
        );
        // A ServiceDown breaks the run of downs, so the streak restarts.
        assert_eq!(
            state.next(ConnEvent::Probe(ProbeOutcome::ServiceDown)),
            Action::Await
        );
        for _ in 0..PROBE_DOWN_THRESHOLD - 1 {
            assert_eq!(
                state.next(ConnEvent::Probe(ProbeOutcome::Down)),
                Action::Await
            );
        }
        assert_eq!(
            state.next(ConnEvent::Probe(ProbeOutcome::Down)),
            Action::Reconnect
        );
    }

    #[test]
    fn probe_down_before_first_connect_is_ignored() {
        let mut state = KeepAliveState::new();
        // No live session yet: a failing probe is the connect not landed, not a
        // zombie. Even a long streak must never reconnect.
        for _ in 0..PROBE_DOWN_THRESHOLD * 2 {
            assert_eq!(
                state.next(ConnEvent::Probe(ProbeOutcome::Down)),
                Action::Await
            );
        }
    }

    #[test]
    fn probe_down_after_session_ends_is_ignored_until_reconnect() {
        let mut state = connected_state();
        // The relay drops: the session is no longer live.
        assert_eq!(sleep_of(state.next(ConnEvent::RelayDropped)), secs(2));
        // Probes arriving before the reconnect lands must not count.
        for _ in 0..PROBE_DOWN_THRESHOLD * 2 {
            assert_eq!(
                state.next(ConnEvent::Probe(ProbeOutcome::Down)),
                Action::Await
            );
        }
        // After reconnecting, the watchdog is armed again from a clean streak.
        assert_eq!(state.next(ConnEvent::Connected), Action::Await);
        for _ in 0..PROBE_DOWN_THRESHOLD - 1 {
            assert_eq!(
                state.next(ConnEvent::Probe(ProbeOutcome::Down)),
                Action::Await
            );
        }
        assert_eq!(
            state.next(ConnEvent::Probe(ProbeOutcome::Down)),
            Action::Reconnect
        );
    }

    #[test]
    fn watchdog_reconnect_rearms_after_a_successful_reconnect() {
        let mut state = connected_state();
        // First zombie reconnect.
        for _ in 0..PROBE_DOWN_THRESHOLD - 1 {
            let _ = state.next(ConnEvent::Probe(ProbeOutcome::Down));
        }
        assert_eq!(
            state.next(ConnEvent::Probe(ProbeOutcome::Down)),
            Action::Reconnect
        );
        // The reconnect lands; the watchdog must require a fresh full streak again.
        assert_eq!(state.next(ConnEvent::Connected), Action::Await);
        for _ in 0..PROBE_DOWN_THRESHOLD - 1 {
            assert_eq!(
                state.next(ConnEvent::Probe(ProbeOutcome::Down)),
                Action::Await
            );
        }
        assert_eq!(
            state.next(ConnEvent::Probe(ProbeOutcome::Down)),
            Action::Reconnect
        );
    }

    #[test]
    fn watchdog_reconnect_does_not_inflate_backoff() {
        let mut state = connected_state();
        for _ in 0..PROBE_DOWN_THRESHOLD - 1 {
            let _ = state.next(ConnEvent::Probe(ProbeOutcome::Down));
        }
        // A watchdog reconnect funnels into the normal connect path; it must not
        // itself bump the backoff. After a successful reconnect, the first ensuing
        // relay drop still sleeps the reset start backoff.
        assert_eq!(
            state.next(ConnEvent::Probe(ProbeOutcome::Down)),
            Action::Reconnect
        );
        assert_eq!(state.next(ConnEvent::Connected), Action::Await);
        assert_eq!(sleep_of(state.next(ConnEvent::RelayDropped)), secs(2));
    }

    #[test]
    fn reconnect_after_drop_changes_phase() {
        let mut state = KeepAliveState::new();
        // Fresh state: first attempt, "Connecting" phase.
        assert!(state.first_attempt());
        assert_eq!(state.phase(), Phase::Initial);
        // After a successful connect, later attempts present as "Reconnecting".
        assert_eq!(state.next(ConnEvent::Connected), Action::Await);
        assert!(!state.first_attempt());
        assert_eq!(state.phase(), Phase::Reconnect);
        // A relay drop schedules the reconnect sleep at the reset backoff.
        assert_eq!(sleep_of(state.next(ConnEvent::RelayDropped)), secs(2));
    }
}
