//! 영구 성능 트레이스의 기록기(docs/spec/performance-trace.md, V5-104).
//!
//! 트레이스는 모든 빌드에 상시 있고 스위치는 서비스 디렉터리의 `performance` 플래그
//! 파일이다. 플래그 파일은 대상 로그 파일의 경로를 한 줄로 담는다(호스트가 설정에서
//! 켤 때 쓴다). 플래그가 없는 동안 이 모듈은 어떤 파일 작업도 하지 않는다 — 열지도,
//! 만들지도, 포맷팅하지도 않는다. 로테이션은 이 파일을 소유하지 않으므로 호스트가
//! 담당하고, 기록기는 이벤트마다 열어 한 줄을 덧붙이고 닫는다(호스트의 로테이션이
//! 즉시 반영되게 한다).

use serde_json::{json, Map, Value};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 서비스 디렉터리의 플래그 파일을 읽어 만든 트레이스.
#[derive(Clone)]
pub struct PerformanceTrace {
    target: Option<PathBuf>,
}

impl PerformanceTrace {
    /// 서비스 디렉터리에서 플래그를 다시 읽는다. 세션을 열 때마다 새로 만들어
    /// 설정 변경이 다음 세션에 반영되게 한다.
    pub fn from_service_dir(service_dir: &Path) -> Self {
        let target = std::fs::read_to_string(service_dir.join("performance"))
            .ok()
            .map(|text| text.trim().to_string())
            .filter(|line| line.starts_with('/'))
            .map(PathBuf::from);
        Self { target }
    }

    /// 꺼진 트레이스. 검사 하네스의 serve 경로가 쓴다.
    pub fn disabled() -> Self {
        Self { target: None }
    }

    /// 트레이스가 켜져 있는가.
    pub fn enabled(&self) -> bool {
        self.target.is_some()
    }

    /// 한 이벤트 줄을 덧붙인다. 꺼져 있으면 아무 일도 하지 않는다.
    pub fn line(&self, event: &str, fields: Value) {
        let Some(target) = self.target.as_ref() else {
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
        // 계기 쓰기 실패는 관측 대상이 아니다: 파이프가 끊기거나 디스크가 찼을 수 있다.
        drop(writeln!(file, "{}", Value::Object(record)));
    }
}

/// 유닉스 시각(밀리초)을 ISO-8601 로 바꾼다(종속성을 더하지 않고 직접 계산).
fn now_iso8601_ms() -> String {
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
