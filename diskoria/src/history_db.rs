//! Pro-Monitoring: SQLite persistence for drive health history.
//!
//! Database location: `%ProgramData%\Diskoria\history.db`

use crate::monitor::HealthSnapshot;
use rusqlite::{params, Connection, Result};
use std::path::PathBuf;

// ── Path ─────────────────────────────────────────────────────────────────────

pub fn db_path() -> PathBuf {
    crate::paths::history_db_file()
}

// ── Open / create ─────────────────────────────────────────────────────────────

/// Open (or create) the history database, applying schema migrations.
pub fn open_or_create() -> Result<Connection> {
    open_rw_at(&db_path(), "WAL")
}

/// Open (or create) the system database the root monitoring service writes.
///
/// Uses a rollback journal rather than WAL **on purpose**: a WAL reader has to
/// create and write the `-shm` sidecar, which an unprivileged desktop session
/// cannot do in a root-owned directory. The whole point of this database is
/// that the service writes it as root and the session reads it as the user, so
/// the journal mode has to be one that read-only readers can use.
#[cfg(target_os = "linux")]
pub fn open_system_rw() -> Result<Connection> {
    let path = crate::paths::system_history_db_file();
    let conn = open_rw_at(&path, "TRUNCATE")?;
    // World-readable so any desktop session can read health data; writing
    // stays root-only through the directory's own permissions.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
    Ok(conn)
}

/// Open the service's system database read-only. `None` when the service is
/// not installed, has never run, or the file is unreadable — every caller
/// treats that as "no service, fall back to polling ourselves".
#[cfg(target_os = "linux")]
pub fn open_system_readonly() -> Option<Connection> {
    let path = crate::paths::system_history_db_file();
    if !path.exists() {
        return None;
    }
    let conn = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| log::debug!(target: "diskoria::monitor", "system history db unreadable: {e}"))
    .ok()?;
    let _ = conn.execute_batch("PRAGMA busy_timeout=5000;");
    Some(conn)
}

fn open_rw_at(path: &std::path::Path, journal: &str) -> Result<Connection> {
    // Ensure the parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let conn = Connection::open(path)?;

    conn.execute_batch(&format!("PRAGMA journal_mode={journal};"))?;
    conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
    conn.execute_batch("PRAGMA busy_timeout=5000;")?;

    apply_schema(&conn)?;

    Ok(conn)
}

/// Schema setup for tests in other modules that build a throwaway database.
#[cfg(test)]
pub(crate) fn apply_schema_for_tests(conn: &Connection) -> Result<()> {
    apply_schema(conn)
}

fn apply_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS health_snapshots (
            id                     INTEGER PRIMARY KEY AUTOINCREMENT,
            serial                 TEXT    NOT NULL,
            model                  TEXT    NOT NULL,
            timestamp              INTEGER NOT NULL,
            temp_c                 INTEGER,
            reallocated_sectors    INTEGER,
            pending_sectors        INTEGER,
            uncorrectable_sectors  INTEGER,
            wear_pct               INTEGER,
            available_spare_pct    INTEGER,
            available_spare_thresh INTEGER,
            critical_warning       INTEGER,
            raw_json               TEXT    NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_snapshots_serial_ts
            ON health_snapshots (serial, timestamp DESC);

        CREATE TABLE IF NOT EXISTS alert_cooldowns (
            serial          TEXT NOT NULL,
            condition_key   TEXT NOT NULL,
            last_fired      INTEGER NOT NULL,
            PRIMARY KEY (serial, condition_key)
        );

        CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER NOT NULL
        );

        INSERT OR IGNORE INTO schema_version (version) VALUES (1);
        ",
    )
}

// ── Insert ────────────────────────────────────────────────────────────────────

pub fn insert_snapshot(conn: &Connection, snap: &HealthSnapshot) -> Result<()> {
    conn.execute(
        "INSERT INTO health_snapshots
            (serial, model, timestamp, temp_c,
             reallocated_sectors, pending_sectors, uncorrectable_sectors,
             wear_pct, available_spare_pct, available_spare_thresh,
             critical_warning, raw_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            snap.serial,
            snap.model,
            snap.timestamp_unix,
            snap.temp_c,
            snap.reallocated_sectors.map(|v| v as i64),
            snap.pending_sectors.map(|v| v as i64),
            snap.uncorrectable_sectors.map(|v| v as i64),
            snap.wear_pct.map(|v| v as i32),
            snap.available_spare_pct.map(|v| v as i32),
            snap.available_spare_threshold.map(|v| v as i32),
            snap.critical_warning.map(|v| v as i32),
            snap.raw_json,
        ],
    )?;
    Ok(())
}

// ── Query ─────────────────────────────────────────────────────────────────────

/// Returns `(unix_timestamp, temp_c)` pairs for the given drive over the last
/// `hours` hours, ordered oldest-first.
pub fn query_temperature_history(
    conn: &Connection,
    serial: &str,
    hours: u32,
) -> Result<Vec<(i64, i32)>> {
    let cutoff = chrono::Utc::now().timestamp() - (hours as i64 * 3600);
    let mut stmt = conn.prepare(
        "SELECT timestamp, temp_c
           FROM health_snapshots
          WHERE serial = ?1
            AND timestamp >= ?2
            AND temp_c IS NOT NULL
          ORDER BY timestamp ASC",
    )?;
    let rows = stmt.query_map(params![serial, cutoff], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i32>(1)?))
    })?;
    rows.collect()
}

/// Load the most recent snapshot for the given drive serial.
pub fn load_last_snapshot(conn: &Connection, serial: &str) -> Result<Option<HealthSnapshot>> {
    let mut stmt = conn.prepare(
        "SELECT serial, model, timestamp, temp_c,
                reallocated_sectors, pending_sectors, uncorrectable_sectors,
                wear_pct, available_spare_pct, available_spare_thresh,
                critical_warning, raw_json
           FROM health_snapshots
          WHERE serial = ?1
          ORDER BY timestamp DESC
          LIMIT 1",
    )?;

    let mut rows = stmt.query_map(params![serial], |row| {
        Ok(HealthSnapshot {
            serial: row.get(0)?,
            model: row.get(1)?,
            timestamp_unix: row.get(2)?,
            temp_c: row.get(3)?,
            reallocated_sectors: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
            pending_sectors: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
            uncorrectable_sectors: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
            wear_pct: row.get::<_, Option<i32>>(7)?.map(|v| v as u8),
            available_spare_pct: row.get::<_, Option<i32>>(8)?.map(|v| v as u8),
            available_spare_threshold: row.get::<_, Option<i32>>(9)?.map(|v| v as u8),
            critical_warning: row.get::<_, Option<i32>>(10)?.map(|v| v as u8),
            raw_json: row.get(11)?,
        })
    })?;

    rows.next().transpose()
}

// ── Prune ─────────────────────────────────────────────────────────────────────

/// Delete snapshots older than 90 days.
pub fn prune_old_records(conn: &Connection) -> Result<()> {
    // 90 days = 7_776_000 seconds
    conn.execute(
        "DELETE FROM health_snapshots WHERE timestamp < unixepoch() - 7776000",
        [],
    )?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").ok();
        apply_schema(&conn).unwrap();
        conn
    }

    fn make_snap(serial: &str, temp: i32, ts: i64) -> HealthSnapshot {
        HealthSnapshot {
            serial: serial.to_string(),
            model: "Test Drive".to_string(),
            timestamp_unix: ts,
            temp_c: Some(temp),
            reallocated_sectors: Some(0),
            pending_sectors: Some(0),
            uncorrectable_sectors: Some(0),
            wear_pct: Some(10),
            available_spare_pct: None,
            available_spare_threshold: None,
            critical_warning: None,
            raw_json: r#"{"type":"test"}"#.to_string(),
        }
    }

    #[test]
    fn insert_and_query() {
        let conn = open_test_db();
        let snap = make_snap("SN123", 42, 1_000_000);
        insert_snapshot(&conn, &snap).unwrap();

        let rows = query_temperature_history(&conn, "SN123", 999_999).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, 42);
    }

    #[test]
    fn load_last_snapshot_returns_most_recent() {
        let conn = open_test_db();
        insert_snapshot(&conn, &make_snap("SN456", 30, 1_000)).unwrap();
        insert_snapshot(&conn, &make_snap("SN456", 55, 2_000)).unwrap();

        let last = load_last_snapshot(&conn, "SN456").unwrap().unwrap();
        assert_eq!(last.temp_c, Some(55));
    }

    #[test]
    fn prune_removes_old_rows() {
        let conn = open_test_db();
        // Insert a very old snapshot (90+ days ago)
        let old_ts = chrono::Utc::now().timestamp() - 8_000_000;
        insert_snapshot(&conn, &make_snap("SN789", 40, old_ts)).unwrap();

        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM health_snapshots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, 1);

        prune_old_records(&conn).unwrap();

        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM health_snapshots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, 0);
    }
}
