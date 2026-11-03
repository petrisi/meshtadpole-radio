//! SQLite helpers and tables common to all tools: meta, sessions, radio_stats, radio_events, requests.
//! Tools add their own tables on top (`open_rw(path, tool_schema, version)`).

use crate::link::{self, Stats};
use crate::sx1262::LoraConfig;
use rusqlite::{Connection, OpenFlags, params, types::ValueRef};
use serde_json::{Map, Value, json};
use std::path::Path;

pub use rusqlite;

pub const COMMON_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);

CREATE TABLE IF NOT EXISTS sessions(
  id INTEGER PRIMARY KEY,
  started REAL NOT NULL,
  last_alive REAL NOT NULL,
  ended REAL,
  tool_version TEXT,
  usb_serial TEXT,
  freq_hz INTEGER, bw_hz INTEGER, sf INTEGER, cr INTEGER,
  config_json TEXT
);

CREATE TABLE IF NOT EXISTS radio_stats(
  id INTEGER PRIMARY KEY,
  session_id INTEGER,
  t_start REAL NOT NULL, t_end REAL NOT NULL,
  noise_samples INTEGER, noise_min REAL, noise_avg REAL, noise_max REAL,
  frames INTEGER, crc_errors INTEGER, header_errors INTEGER, rx_airtime_ms REAL
);
CREATE INDEX IF NOT EXISTS radio_stats_time ON radio_stats(t_start);

CREATE TABLE IF NOT EXISTS radio_events(
  id INTEGER PRIMARY KEY,
  session_id INTEGER,
  t REAL NOT NULL,
  event TEXT NOT NULL,
  detail TEXT
);

CREATE TABLE IF NOT EXISTS requests(
  id INTEGER PRIMARY KEY,
  t REAL NOT NULL,
  request_json TEXT,
  result_json TEXT
);
"#;

pub type Result<T> = std::result::Result<T, rusqlite::Error>;

pub fn now() -> f64 {
    link::now()
}

/// Open read-write, create common + tool tables, record schema version.
pub fn open_rw(path: &Path, tool_schema: &str, version: i64) -> std::result::Result<Connection, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let c = Connection::open(path)?;
    c.pragma_update(None, "journal_mode", "WAL")?;
    c.pragma_update(None, "synchronous", "NORMAL")?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    c.execute_batch(COMMON_SCHEMA)?;
    c.execute_batch(tool_schema)?;
    c.execute(
        "INSERT INTO meta(key,value) VALUES('schema_version',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![version.to_string()],
    )?;
    Ok(c)
}

/// Open read-only (CLI queries while the daemon writes).
pub fn open_ro(path: &Path) -> std::result::Result<Connection, Box<dyn std::error::Error + Send + Sync>> {
    if !path.exists() {
        return Err(format!("database not found: {}", path.display()).into());
    }
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(c)
}

/// Run a query, rows as JSON objects. BLOBs become lowercase hex; TEXT columns named `*_json` are parsed.
pub fn query_json(c: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Value>> {
    let mut st = c.prepare(sql)?;
    let cols: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = st.query(args)?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let mut m = Map::new();
        for (i, name) in cols.iter().enumerate() {
            let v = match r.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(x) => json!(x),
                ValueRef::Real(x) => json!(x),
                ValueRef::Text(t) => {
                    let s = String::from_utf8_lossy(t);
                    if name.ends_with("_json") {
                        serde_json::from_str(&s).unwrap_or(Value::String(s.into_owned()))
                    } else {
                        Value::String(s.into_owned())
                    }
                }
                ValueRef::Blob(b) => Value::String(hex::encode(b)),
            };
            m.insert(name.clone(), v);
        }
        out.push(Value::Object(m));
    }
    Ok(out)
}

/// Daemon-side connection with the current session.
pub struct Session {
    pub c: Connection,
    pub id: i64,
}

impl Session {
    pub fn start(c: Connection, tool_version: &str, usb_serial: &str, lora: &LoraConfig, cfg_json: &str) -> Result<Session> {
        let t = now();
        c.execute(
            "INSERT INTO sessions(started,last_alive,tool_version,usb_serial,freq_hz,bw_hz,sf,cr,config_json) VALUES(?1,?1,?2,?3,?4,?5,?6,?7,?8)",
            params![t, tool_version, usb_serial, lora.freq_hz, lora.bw_hz, lora.sf, lora.cr, cfg_json],
        )?;
        Ok(Session { id: c.last_insert_rowid(), c })
    }

    pub fn heartbeat(&self) -> Result<()> {
        self.c.execute("UPDATE sessions SET last_alive=?1 WHERE id=?2", params![now(), self.id])?;
        Ok(())
    }

    pub fn end(&self) -> Result<()> {
        self.c.execute("UPDATE sessions SET last_alive=?1, ended=?1 WHERE id=?2", params![now(), self.id])?;
        Ok(())
    }

    /// Radio lifecycle: connected, disconnected, open_failed, rx_restored.
    pub fn radio_event(&self, event: &str, detail: &str) -> Result<()> {
        self.c.execute(
            "INSERT INTO radio_events(session_id,t,event,detail) VALUES(?1,?2,?3,?4)",
            params![self.id, now(), event, detail],
        )?;
        if event == "connected" {
            self.c.execute("UPDATE sessions SET usb_serial=?1 WHERE id=?2 AND (usb_serial IS NULL OR usb_serial='')", params![detail, self.id])?;
        }
        Ok(())
    }

    pub fn radio_stats(&self, s: &Stats) -> Result<()> {
        self.c.execute(
            "INSERT INTO radio_stats(session_id,t_start,t_end,noise_samples,noise_min,noise_avg,noise_max,frames,crc_errors,header_errors,rx_airtime_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                self.id, s.t_start, s.t_end, s.noise_samples, s.noise_min, s.noise_avg, s.noise_max,
                s.frames, s.crc_errors, s.header_errors, s.rx_airtime_ms
            ],
        )?;
        Ok(())
    }
}

/// Log an API request and its result.
pub fn log_request(c: &Connection, req: &Value, result: &Value) -> Result<()> {
    c.execute(
        "INSERT INTO requests(t,request_json,result_json) VALUES(?1,?2,?3)",
        params![now(), req.to_string(), result.to_string()],
    )?;
    Ok(())
}
