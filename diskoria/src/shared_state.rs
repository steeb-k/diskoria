//! Process-wide state shared across every Diskoria window.
//!
//! Rule of thumb: anything that must stay in lockstep across open windows
//! (settings, the enumerated drive list, monitor state) lives here; per-window
//! UI transients stay on `DiskoriaApp`. See the multi-window section of
//! `docs/gui-architecture.md`.
//!
//! Locking policy: never hold a guard across an entire `draw()` pass.  Read at
//! the top of the frame, copy primitives or clone cheaply-clonable data into
//! locals, drop the guard, then draw.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use egui::Color32;
use winit::event_loop::EventLoopProxy;

use crate::app_settings::{self, initial_accent_color, Settings};
use crate::detected_drive::DetectedDrive;
use crate::UserEvent;

#[cfg(windows)]
use crate::monitor::MonitorMsg;

pub type DrivePollResult = Result<Vec<DetectedDrive>, String>;

/// Drive enumeration state — one enumeration task services every window.
#[derive(Default)]
pub struct DrivesState {
    pub list: Vec<DetectedDrive>,
    pub loading: bool,
    pub error: Option<String>,
    /// Bumped every time `list` is replaced. Each window tracks the last
    /// generation it applied so all windows converge to the shared list even
    /// when a different window drained the one-shot enumeration receiver.
    pub generation: u64,
}

pub struct SharedAppState {
    settings: RwLock<Settings>,
    accent_color: Mutex<Color32>,
    accent_last_poll: Mutex<Option<Instant>>,
    drives: RwLock<DrivesState>,
    /// Singleton receiver for the one-shot drive-enumeration task.  Lives on
    /// shared state because multi-window mode has many windows but a single
    /// enumeration; the first `poll_drive_enumeration` drains it.
    drive_poll_rx: Mutex<Option<Receiver<DrivePollResult>>>,
    /// When the in-flight enumeration started, for the watchdog that recovers
    /// from a hung WMI query (it would otherwise spin the loading spinner
    /// forever). `None` when no enumeration is in flight.
    drive_poll_started: Mutex<Option<Instant>>,
    /// Singleton receiver for the Pro-Monitoring background thread.
    #[cfg(windows)]
    monitor_rx: Mutex<Option<Receiver<MonitorMsg>>>,
    /// Cancellation flag for the monitor thread; `Some` iff thread is running.
    #[cfg(windows)]
    monitor_cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// Latest health snapshot per drive serial, feeding the tray flyout.
    /// Shared because every open window competitively drains the single
    /// `monitor_rx`; keeping the snapshots here means whichever window drains
    /// a message lands it in one common map, instead of splitting the set
    /// across per-window state (which left the tray flyout showing "-" for
    /// drives whose snapshot happened to go to the other window).
    #[cfg(windows)]
    last_snapshots: Mutex<std::collections::HashMap<String, crate::monitor::HealthSnapshot>>,
    /// Set to `true` when the drive list changes; cleared by `about_to_wait`
    /// after the tray rebuilds its per-drive icons.  Singleton so that a
    /// second window opening does not retrigger a rebuild that would wipe
    /// the temperature rendering already filled in by the monitor thread.
    drive_icons_dirty: AtomicBool,
    /// Set once the startup update check has been kicked off, so it fires a
    /// single time per process no matter how many windows draw (or how often
    /// the first one repaints).
    #[cfg(windows)]
    auto_update_check_started: AtomicBool,
    /// A downloaded installer waiting to be run when the process exits. Lives
    /// here rather than on a window because the window that staged it may be
    /// closed long before the app quits, and `App::exiting` is what applies it.
    #[cfg(windows)]
    staged_update: Mutex<Option<std::path::PathBuf>>,
    /// Physical drives currently under test, keyed by the owning window's token
    /// → that drive's `DetectedDrive::lock_key()`. One test per physical drive
    /// across all windows; dropdowns gray out drives locked by *other* windows.
    /// A window publishes its lock every frame and clears it on close.
    test_locks: Mutex<std::collections::HashMap<u64, String>>,
    pub pro_edition: bool,
    pub event_proxy: EventLoopProxy<UserEvent>,
}

impl SharedAppState {
    pub fn new(event_proxy: EventLoopProxy<UserEvent>) -> Self {
        let settings = app_settings::load_settings();
        let accent_color = initial_accent_color(&settings);
        Self {
            settings: RwLock::new(settings),
            accent_color: Mutex::new(accent_color),
            accent_last_poll: Mutex::new(None),
            drives: RwLock::new(DrivesState {
                list: Vec::new(),
                loading: true,
                error: None,
                generation: 0,
            }),
            drive_poll_rx: Mutex::new(None),
            drive_poll_started: Mutex::new(None),
            #[cfg(windows)]
            monitor_rx: Mutex::new(None),
            #[cfg(windows)]
            monitor_cancel: Mutex::new(None),
            #[cfg(windows)]
            last_snapshots: Mutex::new(std::collections::HashMap::new()),
            drive_icons_dirty: AtomicBool::new(false),
            #[cfg(windows)]
            auto_update_check_started: AtomicBool::new(false),
            #[cfg(windows)]
            staged_update: Mutex::new(None),
            test_locks: Mutex::new(std::collections::HashMap::new()),
            pro_edition: true,
            event_proxy,
        }
    }

    // ── Settings ─────────────────────────────────────────────────────────────

    /// Copy of the current settings.  Use this over holding the read guard
    /// when you need to make a lot of decisions in `draw()`.
    pub fn settings_snapshot(&self) -> Settings {
        self.settings.read().expect("settings poisoned").clone()
    }

    /// Mutate settings under the write lock, persist to disk, and
    /// broadcast `SettingsChanged` to every window.  `restart_monitor`
    /// is auto-detected from monitor-relevant field diffs (threshold or
    /// cadence changes, or the on/off toggle) — the handler in
    /// `App::user_event` cancels the thread so the next draw can respawn
    /// it with fresh thresholds.
    pub fn update_settings<R>(&self, f: impl FnOnce(&mut Settings) -> R) -> R {
        #[cfg_attr(not(windows), allow(unused_variables))]
        let (r, restart_monitor) = {
            let mut guard = self.settings.write().expect("settings poisoned");
            let before = guard.clone();
            let r = f(&mut guard);
            let restart_monitor = before.monitoring_enabled != guard.monitoring_enabled
                || before.poll_interval_mins != guard.poll_interval_mins
                || before.alert_temp_warn != guard.alert_temp_warn
                || before.alert_temp_critical != guard.alert_temp_critical
                || before.alert_wear_threshold != guard.alert_wear_threshold;
            app_settings::save_settings(&guard);
            (r, restart_monitor)
        };
        #[cfg(windows)]
        {
            let _ = self
                .event_proxy
                .send_event(crate::UserEvent::SettingsChanged { restart_monitor });
        }
        r
    }

    // ── Accent ───────────────────────────────────────────────────────────────

    pub fn accent_color(&self) -> Color32 {
        *self.accent_color.lock().expect("accent_color poisoned")
    }

    pub fn set_accent_color(&self, c: Color32) {
        *self.accent_color.lock().expect("accent_color poisoned") = c;
    }

    pub fn accent_last_poll(&self) -> Option<Instant> {
        *self.accent_last_poll.lock().expect("accent_last_poll poisoned")
    }

    pub fn set_accent_last_poll(&self, t: Option<Instant>) {
        *self.accent_last_poll.lock().expect("accent_last_poll poisoned") = t;
    }

    // ── Drives ───────────────────────────────────────────────────────────────

    /// Cheap-ish read: guard is held for the duration of `f`.  Use when you
    /// only need a few fields without cloning the `Vec<DetectedDrive>`.
    pub fn drives_read<R>(&self, f: impl FnOnce(&DrivesState) -> R) -> R {
        f(&self.drives.read().expect("drives poisoned"))
    }

    pub fn drives_write<R>(&self, f: impl FnOnce(&mut DrivesState) -> R) -> R {
        f(&mut self.drives.write().expect("drives poisoned"))
    }

    // ── Drive enumeration receiver ───────────────────────────────────────────

    /// Store a fresh drive-enumeration receiver.  Overwrites any prior
    /// receiver (rare — happens on an explicit refresh).
    pub fn set_drive_poll_rx(&self, rx: Option<Receiver<DrivePollResult>>) {
        *self.drive_poll_rx.lock().expect("drive_poll_rx poisoned") = rx;
    }

    pub fn drive_poll_is_active(&self) -> bool {
        self.drive_poll_rx.lock().expect("drive_poll_rx poisoned").is_some()
    }

    /// True when an enumeration is in flight and has run longer than `timeout`
    /// without delivering — the watchdog uses this to abandon a hung WMI query.
    pub fn drive_poll_timed_out(&self, timeout: Duration) -> bool {
        if !self.drive_poll_is_active() {
            return false;
        }
        self.drive_poll_started
            .lock()
            .expect("drive_poll_started poisoned")
            .is_some_and(|t| t.elapsed() >= timeout)
    }

    /// Abandon the in-flight enumeration (drop the receiver and timer). A late
    /// result from a hung worker thread is harmlessly discarded.
    pub fn cancel_drive_poll(&self) {
        *self.drive_poll_rx.lock().expect("drive_poll_rx poisoned") = None;
        *self.drive_poll_started.lock().expect("drive_poll_started poisoned") = None;
    }

    /// Current generation of the shared drive list.
    pub fn drives_generation(&self) -> u64 {
        self.drives.read().expect("drives poisoned").generation
    }

    /// Non-blocking drain.  Returns `Some(result)` when the enumeration
    /// thread has delivered, clearing the receiver afterwards.  Returns
    /// `None` while still pending; returns `None` and clears the receiver
    /// on channel disconnect.
    pub fn try_recv_drive_poll(&self) -> Option<DrivePollResult> {
        let mut guard = self.drive_poll_rx.lock().expect("drive_poll_rx poisoned");
        let result = {
            let Some(rx) = &*guard else { return None };
            rx.try_recv()
        };
        match result {
            Ok(v) => {
                *guard = None;
                Some(v)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                *guard = None;
                None
            }
        }
    }

    // ── Drive-icon dirty bit (singleton) ─────────────────────────────────────

    /// Signal that the drive list has changed and the tray should rebuild
    /// its per-drive icons.  Safe to call from any window; a single
    /// `about_to_wait` tick consumes the flag.
    pub fn mark_drive_icons_dirty(&self) {
        self.drive_icons_dirty.store(true, Ordering::SeqCst);
    }

    /// Atomically read-and-clear the drive-icon dirty flag.
    pub fn take_drive_icons_dirty(&self) -> bool {
        self.drive_icons_dirty.swap(false, Ordering::SeqCst)
    }

    // ── Startup update check (singleton) ─────────────────────────────────────

    /// Claim the once-per-process startup update check. Returns `true` exactly
    /// once — to the first caller — so only one window fires it.
    #[cfg(windows)]
    pub fn claim_auto_update_check(&self) -> bool {
        !self.auto_update_check_started.swap(true, Ordering::SeqCst)
    }

    // ── Staged update ────────────────────────────────────────────────────────

    /// Record a downloaded installer to run at process exit.
    #[cfg(windows)]
    pub fn stage_update(&self, installer: std::path::PathBuf) {
        *self.staged_update.lock().expect("staged_update poisoned") = Some(installer);
    }

    /// Take the staged installer, if any. Called once from `App::exiting`; also
    /// used by "Update now" so the exit hook doesn't launch a second copy.
    #[cfg(windows)]
    pub fn take_staged_update(&self) -> Option<std::path::PathBuf> {
        self.staged_update
            .lock()
            .expect("staged_update poisoned")
            .take()
    }

    // ── Per-drive test locks (cross-window) ──────────────────────────────────

    /// Wake every window so dropdowns re-evaluate which drives are locked. Sent
    /// only when a lock actually changes (test start/stop/close), so this is rare.
    fn broadcast_repaint(&self) {
        let _ = self
            .event_proxy
            .send_event(crate::UserEvent::Repaint { after: Duration::ZERO });
    }

    /// Publish this window's "drive under test" (or `None` when idle). Called
    /// every frame; only writes + broadcasts when the value changed.
    pub fn set_window_test_lock(&self, token: u64, key: Option<String>) {
        let mut guard = self.test_locks.lock().expect("test_locks poisoned");
        let changed = match &key {
            Some(k) => guard.get(&token) != Some(k),
            None => guard.contains_key(&token),
        };
        if !changed {
            return;
        }
        match key {
            Some(k) => {
                guard.insert(token, k);
            }
            None => {
                guard.remove(&token);
            }
        }
        drop(guard);
        self.broadcast_repaint();
    }

    /// Remove a window's lock entirely (on window close). Broadcasts if it held one.
    pub fn clear_window_test_lock(&self, token: u64) {
        let removed = self
            .test_locks
            .lock()
            .expect("test_locks poisoned")
            .remove(&token)
            .is_some();
        if removed {
            self.broadcast_repaint();
        }
    }

    /// True when `key` is locked by some window other than `token`.
    pub fn drive_busy_elsewhere(&self, token: u64, key: &str) -> bool {
        self.test_locks
            .lock()
            .expect("test_locks poisoned")
            .iter()
            .any(|(t, k)| *t != token && k == key)
    }

    /// All drive keys locked by windows other than `token`.
    pub fn busy_keys_excluding(&self, token: u64) -> std::collections::HashSet<String> {
        self.test_locks
            .lock()
            .expect("test_locks poisoned")
            .iter()
            .filter(|(t, _)| **t != token)
            .map(|(_, k)| k.clone())
            .collect()
    }

    // ── Monitor thread lifecycle ─────────────────────────────────────────────

    #[cfg(windows)]
    pub fn monitor_is_running(&self) -> bool {
        self.monitor_cancel.lock().expect("monitor_cancel poisoned").is_some()
    }

    #[cfg(windows)]
    pub fn set_monitor_running(
        &self,
        rx: Receiver<MonitorMsg>,
        cancel: Arc<AtomicBool>,
    ) {
        *self.monitor_rx.lock().expect("monitor_rx poisoned") = Some(rx);
        *self.monitor_cancel.lock().expect("monitor_cancel poisoned") = Some(cancel);
    }

    /// Flip the cancellation flag and drop the receiver.  Safe to call when
    /// no monitor is running.
    #[cfg(windows)]
    pub fn cancel_monitor(&self) {
        if let Some(c) = self.monitor_cancel.lock().expect("monitor_cancel poisoned").take() {
            c.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        *self.monitor_rx.lock().expect("monitor_rx poisoned") = None;
    }

    /// Non-blocking drain of the monitor receiver.  Returns `(msgs, still_connected)`.
    /// Store the latest snapshot for a drive (called from `poll_monitor` in
    /// whichever window drained it).
    #[cfg(windows)]
    pub fn insert_snapshot(&self, snap: crate::monitor::HealthSnapshot) {
        self.last_snapshots
            .lock()
            .expect("last_snapshots poisoned")
            .insert(snap.serial.clone(), snap);
    }

    /// Clone of the latest snapshot for a drive, if any (read by the tray flyout).
    #[cfg(windows)]
    pub fn snapshot_for(&self, serial: &str) -> Option<crate::monitor::HealthSnapshot> {
        self.last_snapshots
            .lock()
            .expect("last_snapshots poisoned")
            .get(serial)
            .cloned()
    }

    /// On disconnect, clears both `monitor_rx` and `monitor_cancel` internally.
    #[cfg(windows)]
    pub fn drain_monitor_rx(&self) -> (Vec<MonitorMsg>, bool) {
        let mut msgs = Vec::new();
        let mut connected = true;
        {
            let guard = self.monitor_rx.lock().expect("monitor_rx poisoned");
            let Some(rx) = &*guard else { return (msgs, false) };
            loop {
                match rx.try_recv() {
                    Ok(m) => msgs.push(m),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        connected = false;
                        break;
                    }
                }
            }
        }
        if !connected {
            *self.monitor_rx.lock().expect("monitor_rx poisoned") = None;
            *self.monitor_cancel.lock().expect("monitor_cancel poisoned") = None;
        }
        (msgs, connected)
    }

    /// Spawn the drive enumeration thread and store its receiver.  Overwrites
    /// any currently-pending receiver — callers use this for both initial
    /// kickoff and explicit refresh.
    pub fn start_drive_enumeration(&self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();

        // `--demo-drives`: hand back the invented machine instead of querying
        // WMI, so a capture run neither needs real hardware nor shows any.
        if crate::demo::config().drives {
            let _ = tx.send(Ok(crate::demo::drives()));
            ctx.request_repaint();
            self.set_drive_poll_rx(Some(rx));
            *self.drive_poll_started.lock().expect("drive_poll_started poisoned") =
                Some(Instant::now());
            return;
        }

        #[cfg(windows)]
        {
            let ctx2 = ctx.clone();
            std::thread::spawn(move || {
                let result = crate::drive_enumeration::enumerate_physical_disks();
                let _ = tx.send(result);
                ctx2.request_repaint();
            });
        }
        #[cfg(not(windows))]
        {
            let _ = tx.send(Err(
                "Drive enumeration runs on Windows only.".to_string(),
            ));
            ctx.request_repaint();
        }
        self.set_drive_poll_rx(Some(rx));
        *self.drive_poll_started.lock().expect("drive_poll_started poisoned") = Some(Instant::now());
    }

    /// Idempotent helper for additional windows: kicks off the one-shot
    /// enumeration only if no receiver is currently in flight and no
    /// drives have been enumerated yet.
    pub fn ensure_drive_enumeration(&self, ctx: &egui::Context) {
        if self.drive_poll_is_active() {
            return;
        }
        let already_loaded = self.drives_read(|d| !d.list.is_empty());
        if already_loaded {
            return;
        }
        self.start_drive_enumeration(ctx);
    }
}
