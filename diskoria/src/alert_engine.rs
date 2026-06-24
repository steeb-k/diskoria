//! Pro-Monitoring: alert condition checking — pure logic, no I/O.

use std::collections::HashMap;

// ── Alert types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AlertCondition {
    TemperatureWarning,
    TemperatureCritical,
    NvmeCriticalWarning,
    NewReallocatedSectors,
    NewPendingSectors,
    NewUncorrectableSectors,
    WearHigh,
    AvailableSpareLow,
}

impl AlertCondition {
    pub fn key(&self) -> &'static str {
        match self {
            AlertCondition::TemperatureWarning => "temp_warn",
            AlertCondition::TemperatureCritical => "temp_crit",
            AlertCondition::NvmeCriticalWarning => "nvme_crit",
            AlertCondition::NewReallocatedSectors => "reallocated",
            AlertCondition::NewPendingSectors => "pending",
            AlertCondition::NewUncorrectableSectors => "uncorrectable",
            AlertCondition::WearHigh => "wear_high",
            AlertCondition::AvailableSpareLow => "spare_low",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertLevel {
    Warning,
    Critical,
}

#[derive(Debug, Clone)]
pub struct AlertEvent {
    pub serial: String,
    pub model: String,
    pub condition: AlertCondition,
    pub level: AlertLevel,
    pub detail: String,
}

// ── Cooldown tracker ──────────────────────────────────────────────────────────

/// Tracks when each (serial, condition) pair was last alerted.
/// Persisted in the `alert_cooldowns` SQLite table across restarts.
pub struct AlertCooldownTracker {
    /// key = (serial, condition_key), value = Unix timestamp of last fire
    map: HashMap<(String, String), i64>,
}

impl AlertCooldownTracker {
    /// Load initial state from the DB.  Fails silently (empty map) on DB error.
    pub fn new(conn: &rusqlite::Connection) -> Self {
        let map = load_cooldowns(conn).unwrap_or_default();
        AlertCooldownTracker { map }
    }

    /// Returns `true` if this condition is still within its cooldown window.
    pub fn is_cooling_down(
        &self,
        serial: &str,
        condition: &AlertCondition,
        now_unix: i64,
        cooldown_secs: i64,
    ) -> bool {
        if let Some(&last) = self.map.get(&(serial.to_string(), condition.key().to_string())) {
            now_unix - last < cooldown_secs
        } else {
            false
        }
    }

    /// Record that an alert fired now and persist to DB.
    pub fn record_fired(
        &mut self,
        serial: &str,
        condition: &AlertCondition,
        now_unix: i64,
        conn: &rusqlite::Connection,
    ) {
        self.map
            .insert((serial.to_string(), condition.key().to_string()), now_unix);
        let _ = save_cooldown(conn, serial, condition.key(), now_unix);
    }
}

fn load_cooldowns(
    conn: &rusqlite::Connection,
) -> rusqlite::Result<HashMap<(String, String), i64>> {
    let mut stmt =
        conn.prepare("SELECT serial, condition_key, last_fired FROM alert_cooldowns")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
    })?;
    let mut map = HashMap::new();
    for row in rows.flatten() {
        map.insert((row.0, row.1), row.2);
    }
    Ok(map)
}

fn save_cooldown(
    conn: &rusqlite::Connection,
    serial: &str,
    condition_key: &str,
    last_fired: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO alert_cooldowns (serial, condition_key, last_fired) VALUES (?1, ?2, ?3)",
        rusqlite::params![serial, condition_key, last_fired],
    )?;
    Ok(())
}

// ── Alert check ───────────────────────────────────────────────────────────────

/// Evaluate all alert conditions for a new snapshot against the previous one.
/// Returns any newly triggered alerts (after cooldown suppression).
pub fn check_alerts(
    new: &crate::monitor::HealthSnapshot,
    prev: Option<&crate::monitor::HealthSnapshot>,
    temp_warn: i32,
    temp_critical: i32,
    wear_threshold: u8,
    cooldowns: &mut AlertCooldownTracker,
    conn: &rusqlite::Connection,
) -> Vec<AlertEvent> {
    let now = chrono::Utc::now().timestamp();
    let mut alerts = Vec::new();

    let mut fire = |condition: AlertCondition, level: AlertLevel, detail: String| {
        let cooldown_secs: i64 = match condition {
            AlertCondition::WearHigh => 86_400, // 24 hours
            _ => 3_600,                          // 1 hour
        };
        if !cooldowns.is_cooling_down(&new.serial, &condition, now, cooldown_secs) {
            cooldowns.record_fired(&new.serial, &condition, now, conn);
            alerts.push(AlertEvent {
                serial: new.serial.clone(),
                model: new.model.clone(),
                condition,
                level,
                detail,
            });
        }
    };

    // Temperature
    if let Some(temp) = new.temp_c {
        if temp >= temp_critical {
            fire(
                AlertCondition::TemperatureCritical,
                AlertLevel::Critical,
                format!("Drive temperature is {}°C (threshold: {}°C)", temp, temp_critical),
            );
        } else if temp >= temp_warn {
            fire(
                AlertCondition::TemperatureWarning,
                AlertLevel::Warning,
                format!("Drive temperature is {}°C (threshold: {}°C)", temp, temp_warn),
            );
        }
    }

    // NVMe critical_warning (only fire when bits are newly set)
    if let Some(cw) = new.critical_warning {
        if cw != 0 {
            let prev_cw = prev.and_then(|p| p.critical_warning).unwrap_or(0);
            let new_bits = cw & !prev_cw;
            if new_bits != 0 {
                fire(
                    AlertCondition::NvmeCriticalWarning,
                    AlertLevel::Critical,
                    format!("NVMe critical warning flags set: 0x{:02X}", cw),
                );
            }
        }
    }

    // NVMe available spare below threshold
    if let (Some(spare), Some(thresh)) = (new.available_spare_pct, new.available_spare_threshold) {
        if spare < thresh {
            fire(
                AlertCondition::AvailableSpareLow,
                AlertLevel::Critical,
                format!(
                    "Available spare {}% is below threshold {}%",
                    spare, thresh
                ),
            );
        }
    }

    // Delta-based sector counts (no cooldown needed — counts rarely decrease)
    let prev_realloc = prev.and_then(|p| p.reallocated_sectors).unwrap_or(0);
    if let Some(realloc) = new.reallocated_sectors {
        if realloc > prev_realloc {
            fire(
                AlertCondition::NewReallocatedSectors,
                AlertLevel::Warning,
                format!(
                    "Reallocated sector count increased: {} → {}",
                    prev_realloc, realloc
                ),
            );
        }
    }

    let prev_pending = prev.and_then(|p| p.pending_sectors).unwrap_or(0);
    if let Some(pending) = new.pending_sectors {
        if pending > prev_pending {
            fire(
                AlertCondition::NewPendingSectors,
                AlertLevel::Warning,
                format!(
                    "Pending sector count increased: {} → {}",
                    prev_pending, pending
                ),
            );
        }
    }

    let prev_uncorr = prev.and_then(|p| p.uncorrectable_sectors).unwrap_or(0);
    if let Some(uncorr) = new.uncorrectable_sectors {
        if uncorr > prev_uncorr {
            fire(
                AlertCondition::NewUncorrectableSectors,
                AlertLevel::Critical,
                format!(
                    "Uncorrectable sector count increased: {} → {}",
                    prev_uncorr, uncorr
                ),
            );
        }
    }

    // Wear level
    if let Some(wear) = new.wear_pct {
        if wear >= wear_threshold {
            fire(
                AlertCondition::WearHigh,
                AlertLevel::Warning,
                format!("Drive wear level is {}% (threshold: {}%)", wear, wear_threshold),
            );
        }
    }

    alerts
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::HealthSnapshot;

    /// In-memory DB with just the `alert_cooldowns` table the tracker needs.
    fn mem_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE alert_cooldowns (
                serial        TEXT NOT NULL,
                condition_key TEXT NOT NULL,
                last_fired    INTEGER NOT NULL,
                PRIMARY KEY (serial, condition_key)
            );",
        )
        .unwrap();
        conn
    }

    fn snap() -> HealthSnapshot {
        HealthSnapshot {
            serial: "SN1".into(),
            model: "M".into(),
            timestamp_unix: 0,
            temp_c: None,
            reallocated_sectors: None,
            pending_sectors: None,
            uncorrectable_sectors: None,
            wear_pct: None,
            available_spare_pct: None,
            available_spare_threshold: None,
            critical_warning: None,
            raw_json: String::new(),
        }
    }

    #[test]
    fn cooldown_boundaries() {
        let conn = mem_db();
        let mut t = AlertCooldownTracker::new(&conn);
        let cond = AlertCondition::TemperatureWarning;
        assert!(!t.is_cooling_down("SN1", &cond, 1000, 3600));
        t.record_fired("SN1", &cond, 1000, &conn);
        // 1 second before the window closes → still cooling down.
        assert!(t.is_cooling_down("SN1", &cond, 1000 + 3599, 3600));
        // Exactly at the window edge → no longer cooling down.
        assert!(!t.is_cooling_down("SN1", &cond, 1000 + 3600, 3600));
        // A different serial is unaffected.
        assert!(!t.is_cooling_down("SN2", &cond, 1000, 3600));
    }

    #[test]
    fn temp_warning_vs_critical() {
        let conn = mem_db();
        let mut cd = AlertCooldownTracker::new(&conn);

        let mut warm = snap();
        warm.temp_c = Some(65);
        let a = check_alerts(&warm, None, 60, 70, 90, &mut cd, &conn);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].condition, AlertCondition::TemperatureWarning);
        assert_eq!(a[0].level, AlertLevel::Warning);

        // Different condition key (crit) → not suppressed by the warn cooldown.
        let mut hot = snap();
        hot.temp_c = Some(75);
        let a2 = check_alerts(&hot, None, 60, 70, 90, &mut cd, &conn);
        assert_eq!(a2.len(), 1);
        assert_eq!(a2[0].condition, AlertCondition::TemperatureCritical);
        assert_eq!(a2[0].level, AlertLevel::Critical);
    }

    #[test]
    fn cooldown_suppresses_repeat() {
        let conn = mem_db();
        let mut cd = AlertCooldownTracker::new(&conn);
        let mut s = snap();
        s.temp_c = Some(65);
        assert_eq!(check_alerts(&s, None, 60, 70, 90, &mut cd, &conn).len(), 1);
        // Immediate repeat is within the 1h cooldown → suppressed.
        assert!(check_alerts(&s, None, 60, 70, 90, &mut cd, &conn).is_empty());
    }

    #[test]
    fn pending_sectors_fire_only_on_increase() {
        let conn = mem_db();
        let mut cd = AlertCooldownTracker::new(&conn);
        let mut prev = snap();
        prev.pending_sectors = Some(4);

        let mut same = snap();
        same.pending_sectors = Some(4);
        let a = check_alerts(&same, Some(&prev), 60, 70, 90, &mut cd, &conn);
        assert!(a.iter().all(|e| e.condition != AlertCondition::NewPendingSectors));

        let mut more = snap();
        more.pending_sectors = Some(5);
        let a2 = check_alerts(&more, Some(&prev), 60, 70, 90, &mut cd, &conn);
        assert!(a2.iter().any(|e| e.condition == AlertCondition::NewPendingSectors));
    }

    #[test]
    fn wear_over_threshold_fires() {
        let conn = mem_db();
        let mut cd = AlertCooldownTracker::new(&conn);
        let mut s = snap();
        s.wear_pct = Some(95);
        let a = check_alerts(&s, None, 60, 70, 90, &mut cd, &conn);
        assert!(a.iter().any(|e| e.condition == AlertCondition::WearHigh));
    }
}
