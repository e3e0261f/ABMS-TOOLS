use crate::models::LocalDb;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn db_path() -> PathBuf {
    let base = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(env::temp_dir);
    base.join("abms").join("abms_db.json")
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn short_unix_time(unix_secs: u64) -> String {
    let days = (unix_secs / 86400) as i64;
    let rem = unix_secs % 86400;
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;

    let z = days + 719468;
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    format!("{m:02}-{d:02} {hh:02}:{mm:02}")
}

pub fn parse_iso_to_unix(s: &str) -> Option<u64> {
    if s.len() < 19 || s.starts_with("0001") {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: i64 = s[5..7].parse().ok()?;
    let d: i64 = s[8..10].parse().ok()?;
    let hh: i64 = s[11..13].parse().ok()?;
    let mm: i64 = s[14..16].parse().ok()?;
    let ss: i64 = s[17..19].parse().ok()?;
    let y_adj = y - if m <= 2 { 1 } else { 0 };
    let era = y_adj.div_euclid(400);
    let yoe = y_adj.rem_euclid(400);
    let m_adj = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * m_adj + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + hh * 3600 + mm * 60 + ss;
    u64::try_from(secs).ok()
}

pub fn load_db() -> LocalDb {
    let path = db_path();
    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(db) = serde_json::from_str::<LocalDb>(&content) {
            return db;
        }
    }
    LocalDb::default()
}

pub fn save_db(db: &LocalDb) {
    let path = db_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(file) = fs::File::create(&path) {
        let _ = serde_json::to_writer_pretty(file, db);
    }
}

pub fn price_key(item_id: &str, quality: u8) -> String {
    format!("{item_id}|Q{}", quality.max(1))
}
