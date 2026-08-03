//! Event-loop stall watchdog.
//!
//! The UI is single-threaded: winit callbacks, egui's layout pass and the
//! CPU rasterizer all run on the event-loop thread, so *any* blocking call
//! that reaches it freezes every window at once. That failure mode is hard to
//! diagnose after the fact — the symptom (a dead GUI while worker threads keep
//! logging) looks the same no matter which call is at fault, and it only shows
//! up under load that is awkward to reproduce (KI-42, KI-43).
//!
//! So the event loop reports where it is, and a background thread notices when
//! it stops moving. Each callback calls [`enter`] with a phase; the loop calls
//! [`idle`] before parking for the next event. If a non-idle phase lasts longer
//! than the threshold, the watchdog logs which one and for how long — turning
//! "it froze" into "it was parked in `paint:surface-acquire` for 41 s".
//!
//! Cost is two relaxed atomic stores per callback, so it stays on in release
//! builds: the bug only appears on real hardware under real load, which is
//! exactly where an opt-in flag would be switched off.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Where the event-loop thread currently is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Parked waiting for an event — the one state that is *supposed* to last.
    Idle = 0,
    NewEvents,
    WindowEvent,
    UserEvent,
    /// Tray back-end call (D-Bus round trip on Linux).
    TrayUpdate,
    AboutToWait,
    Resumed,
    Exiting,
    /// egui layout + the whole `DiskoriaApp::draw` tree, including `poll_*`.
    PaintUi,
    PaintTessellate,
    PaintDamage,
    /// Acquiring the softbuffer back buffer — blocks on the compositor.
    PaintSurfaceAcquire,
    PaintRasterize,
    PaintPresent,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::NewEvents => "new_events",
            Phase::WindowEvent => "window_event",
            Phase::UserEvent => "user_event",
            Phase::TrayUpdate => "tray_update",
            Phase::AboutToWait => "about_to_wait",
            Phase::Resumed => "resumed",
            Phase::Exiting => "exiting",
            Phase::PaintUi => "paint:ui+poll",
            Phase::PaintTessellate => "paint:tessellate",
            Phase::PaintDamage => "paint:damage",
            Phase::PaintSurfaceAcquire => "paint:surface-acquire",
            Phase::PaintRasterize => "paint:rasterize",
            Phase::PaintPresent => "paint:present",
        }
    }

    fn from_index(i: usize) -> Phase {
        match i {
            1 => Phase::NewEvents,
            2 => Phase::WindowEvent,
            3 => Phase::UserEvent,
            4 => Phase::TrayUpdate,
            5 => Phase::AboutToWait,
            6 => Phase::Resumed,
            7 => Phase::Exiting,
            8 => Phase::PaintUi,
            9 => Phase::PaintTessellate,
            10 => Phase::PaintDamage,
            11 => Phase::PaintSurfaceAcquire,
            12 => Phase::PaintRasterize,
            13 => Phase::PaintPresent,
            _ => Phase::Idle,
        }
    }
}

static PHASE: AtomicUsize = AtomicUsize::new(Phase::Idle as usize);
/// Milliseconds since [`origin`] at which the current phase was entered.
static SINCE_MS: AtomicU64 = AtomicU64::new(0);
/// Bumped on every phase change so the watchdog can tell "still stuck in the
/// same call" from "entered the same phase again".
static SEQ: AtomicU64 = AtomicU64::new(0);

fn origin() -> Instant {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    origin().elapsed().as_millis() as u64
}

/// Record that the event-loop thread has entered `phase`.
#[inline]
pub fn enter(phase: Phase) {
    SINCE_MS.store(now_ms(), Ordering::Relaxed);
    PHASE.store(phase as usize, Ordering::Relaxed);
    SEQ.fetch_add(1, Ordering::Relaxed);
}

/// Record that the event-loop thread is parked waiting for events.
#[inline]
pub fn idle() {
    enter(Phase::Idle);
}

/// Run `f` inside `phase`, restoring the previous phase afterwards.
#[inline]
pub fn scope<R>(phase: Phase, f: impl FnOnce() -> R) -> R {
    let prev = Phase::from_index(PHASE.load(Ordering::Relaxed));
    enter(phase);
    let r = f();
    enter(prev);
    r
}

/// Start the watchdog thread. Idempotent; call once at startup.
///
/// The stall threshold defaults to 1000 ms and can be overridden with
/// `DISKORIA_STALL_MS` (0 disables the watchdog entirely).
pub fn start() {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    STARTED.get_or_init(|| {
        let threshold_ms: u64 = std::env::var("DISKORIA_STALL_MS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(1000);
        if threshold_ms == 0 {
            return;
        }
        origin();
        let _ = std::thread::Builder::new()
            .name("diskoria-watchdog".into())
            .spawn(move || watch(threshold_ms));
    });
}

/// What the watchdog should do with one sample. Split out so the decision is
/// testable without spinning threads or waiting real seconds.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Quiet,
    /// The loop has been stuck in `phase` this long and has not been reported.
    Report(Phase, u64),
    /// A previously reported stall has ended.
    Recovered,
}

fn decide(
    reported: Option<u64>,
    seq: u64,
    phase: Phase,
    stuck_ms: u64,
    threshold_ms: u64,
) -> Action {
    match reported {
        // Same phase entry we already warned about: stay quiet until it moves.
        Some(bad_seq) if bad_seq == seq => Action::Quiet,
        Some(_) => Action::Recovered,
        // `Idle` is the event loop parked waiting for input — not a stall, no
        // matter how long it lasts.
        None if phase != Phase::Idle && stuck_ms >= threshold_ms => {
            Action::Report(phase, stuck_ms)
        }
        None => Action::Quiet,
    }
}

fn watch(threshold_ms: u64) {
    // Poll often enough to time a stall usefully, rarely enough to be free.
    let tick = Duration::from_millis(200);
    // The (seq) of the stall we have already reported, so one freeze produces
    // one warning and one recovery line rather than a line every tick.
    let mut reported: Option<u64> = None;

    loop {
        std::thread::sleep(tick);

        let seq = SEQ.load(Ordering::Relaxed);
        let phase = Phase::from_index(PHASE.load(Ordering::Relaxed));
        let since = SINCE_MS.load(Ordering::Relaxed);
        let stuck_ms = now_ms().saturating_sub(since);

        match decide(reported, seq, phase, stuck_ms, threshold_ms) {
            Action::Quiet => {}
            Action::Report(p, ms) => {
                log::warn!(
                    target: "diskoria::watchdog",
                    "event loop stalled {:.1} s in `{}` — a blocking call is running on the \
                     UI thread; every window is frozen until it returns",
                    ms as f64 / 1000.0,
                    p.name()
                );
                remember_stall(seq, since);
                reported = Some(seq);
            }
            Action::Recovered => {
                let bad_seq = reported.expect("Recovered implies a reported stall");
                log::warn!(
                    target: "diskoria::watchdog",
                    "event loop recovered after ~{:.1} s",
                    stall_len(bad_seq) as f64 / 1000.0
                );
                reported = None;
            }
        }
    }
}

// The recovery line wants the *total* stall length, which is only known once
// the phase changes — by then `SINCE_MS` has been overwritten, so the start of
// the reported stall is kept aside.
static STALL_START_MS: AtomicU64 = AtomicU64::new(0);
static STALL_SEQ: AtomicU64 = AtomicU64::new(0);

fn remember_stall(seq: u64, since_ms: u64) {
    STALL_SEQ.store(seq, Ordering::Relaxed);
    STALL_START_MS.store(since_ms, Ordering::Relaxed);
}

fn stall_len(seq: u64) -> u64 {
    if STALL_SEQ.load(Ordering::Relaxed) == seq {
        now_ms().saturating_sub(STALL_START_MS.load(Ordering::Relaxed))
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_indices_round_trip() {
        for p in [
            Phase::Idle,
            Phase::NewEvents,
            Phase::WindowEvent,
            Phase::UserEvent,
            Phase::TrayUpdate,
            Phase::AboutToWait,
            Phase::Resumed,
            Phase::Exiting,
            Phase::PaintUi,
            Phase::PaintTessellate,
            Phase::PaintDamage,
            Phase::PaintSurfaceAcquire,
            Phase::PaintRasterize,
            Phase::PaintPresent,
        ] {
            assert_eq!(Phase::from_index(p as usize), p, "{}", p.name());
        }
    }

    #[test]
    fn a_short_phase_is_not_a_stall() {
        assert_eq!(
            decide(None, 7, Phase::PaintUi, 300, 1000),
            Action::Quiet
        );
    }

    #[test]
    fn a_long_non_idle_phase_is_reported_once() {
        assert_eq!(
            decide(None, 7, Phase::PaintSurfaceAcquire, 4200, 1000),
            Action::Report(Phase::PaintSurfaceAcquire, 4200)
        );
        // Still the same phase entry — no repeat warning every tick.
        assert_eq!(
            decide(Some(7), 7, Phase::PaintSurfaceAcquire, 9000, 1000),
            Action::Quiet
        );
    }

    #[test]
    fn idle_is_never_a_stall_however_long() {
        assert_eq!(
            decide(None, 7, Phase::Idle, 60 * 60 * 1000, 1000),
            Action::Quiet
        );
    }

    #[test]
    fn moving_on_from_a_reported_stall_reports_recovery() {
        assert_eq!(
            decide(Some(7), 8, Phase::AboutToWait, 5, 1000),
            Action::Recovered
        );
    }

    #[test]
    fn scope_restores_the_previous_phase() {
        enter(Phase::WindowEvent);
        scope(Phase::PaintUi, || {
            assert_eq!(Phase::from_index(PHASE.load(Ordering::Relaxed)), Phase::PaintUi);
        });
        assert_eq!(
            Phase::from_index(PHASE.load(Ordering::Relaxed)),
            Phase::WindowEvent
        );
        idle();
    }
}
