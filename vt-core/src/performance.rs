//! The writer of the permanent performance trace (core docs/spec/performance-trace.md).
//!
//! The trace is present in every build, and its switch is the `performance` flag file of the service directory, which
//! names the log file on one line (the host writes it when the setting turns on). The writer reads the flag at each
//! event; while the flag is absent it neither opens, creates nor formats the log. The host owns the rotation of the
//! log, so the writer opens the log for each event, appends one line and closes it, and a rotation applies at once.

use serde_json::{json, Map, Value};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The trace of a service directory. It reads the flag file at each event, so a connection made while the trace was
/// off writes from the moment the host writes the flag and stops when the host removes it.
#[derive(Clone)]
pub struct PerformanceTrace {
    /// The flag file, or `None` for a trace that never writes.
    flag: Option<PathBuf>,
}

impl PerformanceTrace {
    /// The trace of the flag file of service_dir.
    pub fn from_service_dir(service_dir: &Path) -> Self {
        Self {
            flag: Some(service_dir.join("performance")),
        }
    }

    /// A trace that never writes, which the serve path of the test harness uses.
    pub fn disabled() -> Self {
        Self { flag: None }
    }

    /// The log path that the flag file names now, or `None` while the flag is absent or names no absolute path.
    fn target(&self) -> Option<PathBuf> {
        let flag = self.flag.as_ref()?;
        std::fs::read_to_string(flag)
            .ok()
            .map(|text| text.trim().to_string())
            .filter(|line| line.starts_with('/'))
            .map(PathBuf::from)
    }

    /// Whether the flag names a log now.
    pub fn enabled(&self) -> bool {
        self.target().is_some()
    }

    /// Appends one event line to the log that the flag names now; it writes nothing while the flag is absent.
    pub fn line(&self, event: &str, fields: Value) {
        let Some(target) = self.target() else {
            return;
        };
        let mut record = Map::new();
        record.insert("ts".into(), json!(now_iso8601_ms()));
        record.insert("pid".into(), json!(std::process::id()));
        record.insert("layer".into(), json!("vt-core"));
        record.insert("event".into(), json!(event));
        if let Value::Object(extra) = fields {
            for (key, value) in extra {
                record.insert(key, value);
            }
        }
        let mut file = match OpenOptions::new().create(true).append(true).open(target) {
            Ok(file) => file,
            // 계기는 진단이다: 파일을 못 열면 조용히 넘어간다 — 앱이 계기 때문에 실패하지 않는다.
            Err(_) => return,
        };
        // 한 줄은 한 번의 write 로 쓴다. 여러 thread 가 같은 파일에 덧붙이므로, 나눠 쓰면 줄이 섞인다.
        let mut text = Value::Object(record).to_string();
        text.push('\n');
        // 계기 쓰기 실패는 관측 대상이 아니다: 파이프가 끊기거나 디스크가 찼을 수 있다.
        drop(file.write_all(text.as_bytes()));
    }
}

/// 유닉스 시각(밀리초)을 ISO-8601 로 바꾼다(종속성을 더하지 않고 직접 계산).
pub(crate) fn now_iso8601_ms() -> String {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default(); // 기본값: 시계는 에포크 이전을 돌려주지 않는다
    let millis_total = since.as_millis();
    let days = (millis_total / 86_400_000) as i64;
    let millis_day = (millis_total % 86_400_000) as u32;
    let (year, month, day) = civil_from_days(days);
    let hour = millis_day / 3_600_000;
    let minute = (millis_day % 3_600_000) / 60_000;
    let second = (millis_day % 60_000) / 1_000;
    let millis = millis_day % 1_000;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// 날수를 역·월·일로 바꾼다(Howard Hinnant 의 civil_from_days).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// The most bytes of one chunk that a record holds in full; a longer chunk keeps its length and its first bytes.
const BYTES_RECORDED: usize = 4096;

/// The fields of a record of bytes: the length, the text (the bytes read as UTF-8, with the replacement character for
/// bytes that are not), the same bytes in hexadecimal, and whether the record holds the first bytes only.
pub fn bytes_fields(data: &[u8]) -> Value {
    let kept = &data[..data.len().min(BYTES_RECORDED)];
    let hex: String = kept.iter().map(|byte| format!("{byte:02x}")).collect();
    json!({
        "bytes": data.len(),
        "text": String::from_utf8_lossy(kept),
        "hex": hex,
        "truncated": data.len() > kept.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_converter_matches_a_known_instant() {
        // 2026-09-29T10:20:30.400Z — 유닉스 밀리초 1790665230400.
        let since = std::time::Duration::from_millis(1_790_665_230_400);
        assert_eq!(
            civil_from_days((since.as_millis() / 86_400_000) as i64),
            (2026, 9, 29)
        );
    }
}
