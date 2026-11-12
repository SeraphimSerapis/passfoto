//! Pure request/response helpers for the HTTP API, kept free of I/O so they are unit-testable.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::auth::query_param;
use crate::throttle::{ceil_secs, ThrottleStatus};

pub const DEFAULT_QUALITY: u8 = 85;
pub const PREVIEW_MAX_WIDTH: u32 = 960;
pub const PREVIEW_QUALITY: u8 = 70;
const WIDTH_RANGE: std::ops::RangeInclusive<u32> = 32..=8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotParams {
    pub max_width: Option<u32>,
    pub quality: u8,
}

pub fn parse_snapshot_params(url: &str) -> Result<SnapshotParams, String> {
    let max_width = match query_param(url, "width") {
        None => None,
        Some(raw) => match raw.parse::<u32>() {
            Ok(w) if WIDTH_RANGE.contains(&w) => Some(w),
            _ => {
                return Err(format!(
                    "width must be an integer from {} to {}",
                    WIDTH_RANGE.start(),
                    WIDTH_RANGE.end()
                ))
            }
        },
    };
    let quality = match query_param(url, "quality") {
        None => DEFAULT_QUALITY,
        Some(raw) => match raw.parse::<u8>() {
            Ok(q) if (1..=100).contains(&q) => q,
            _ => return Err("quality must be an integer from 1 to 100".into()),
        },
    };
    Ok(SnapshotParams { max_width, quality })
}

pub struct CameraSummary<'a> {
    pub state: &'static str,
    pub error: Option<&'a str>,
}

pub fn status_json(
    min_interval: Duration,
    throttle: ThrottleStatus,
    camera: &CameraSummary,
) -> String {
    let last_shot_age = match throttle.last_shot_age {
        Some(age) => age.as_secs().to_string(),
        None => "null".into(),
    };
    let camera_error = match camera.error {
        Some(e) => json_string(e),
        None => "null".into(),
    };
    format!(
        concat!(
            "{{\"version\":{},\"min_interval_s\":{},\"cooldown_remaining_s\":{},",
            "\"last_shot_age_s\":{},\"camera\":{},\"camera_error\":{}}}\n"
        ),
        json_string(env!("CARGO_PKG_VERSION")),
        min_interval.as_secs(),
        ceil_secs(throttle.cooldown_remaining),
        last_shot_age,
        json_string(camera.state),
        camera_error,
    )
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// RFC 3339 UTC timestamp with millisecond precision, e.g. `2026-11-12T13:06:12.345Z`.
pub fn rfc3339(t: SystemTime) -> String {
    let since = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let (y, m, d) = civil_from_days(secs / 86_400);
    let tod = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        since.subsec_millis()
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_params_default() {
        assert_eq!(
            parse_snapshot_params("/snapshot?token=x"),
            Ok(SnapshotParams {
                max_width: None,
                quality: DEFAULT_QUALITY
            })
        );
    }

    #[test]
    fn snapshot_params_accept_width_and_quality() {
        assert_eq!(
            parse_snapshot_params("/snapshot?width=640&quality=60"),
            Ok(SnapshotParams {
                max_width: Some(640),
                quality: 60
            })
        );
        assert!(parse_snapshot_params("/snapshot?width=32&quality=1").is_ok());
        assert!(parse_snapshot_params("/snapshot?width=8192&quality=100").is_ok());
    }

    #[test]
    fn snapshot_params_reject_out_of_range() {
        for url in [
            "/snapshot?width=0",
            "/snapshot?width=31",
            "/snapshot?width=8193",
            "/snapshot?width=abc",
            "/snapshot?width=",
            "/snapshot?quality=0",
            "/snapshot?quality=101",
            "/snapshot?quality=high",
        ] {
            assert!(
                parse_snapshot_params(url).is_err(),
                "{url} should be rejected"
            );
        }
    }

    #[test]
    fn status_json_shape() {
        let json = status_json(
            Duration::from_secs(120),
            ThrottleStatus {
                cooldown_remaining: Duration::from_millis(37_200),
                last_shot_age: Some(Duration::from_secs(83)),
            },
            &CameraSummary {
                state: "streaming",
                error: None,
            },
        );
        assert_eq!(
            json,
            format!(
                "{{\"version\":\"{}\",\"min_interval_s\":120,\"cooldown_remaining_s\":38,\
                 \"last_shot_age_s\":83,\"camera\":\"streaming\",\"camera_error\":null}}\n",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn status_json_before_first_shot_and_with_error() {
        let json = status_json(
            Duration::from_secs(5),
            ThrottleStatus {
                cooldown_remaining: Duration::ZERO,
                last_shot_age: None,
            },
            &CameraSummary {
                state: "failed",
                error: Some("no \"camera\"\n"),
            },
        );
        assert!(json.contains("\"cooldown_remaining_s\":0"));
        assert!(json.contains("\"last_shot_age_s\":null"));
        assert!(json.contains("\"camera\":\"failed\",\"camera_error\":\"no \\\"camera\\\"\\n\""));
    }

    #[test]
    fn json_string_escapes_controls() {
        assert_eq!(json_string("a\u{1}b\\"), "\"a\\u0001b\\\\\"");
    }

    #[test]
    fn rfc3339_formats_known_instants() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let t = UNIX_EPOCH + Duration::from_millis(1_709_210_096_789);
        assert_eq!(rfc3339(t), "2024-02-29T12:34:56.789Z");
        let t = UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(rfc3339(t), "2000-02-29T00:00:00.000Z");
        let t = UNIX_EPOCH + Duration::from_secs(4_102_444_799);
        assert_eq!(rfc3339(t), "2099-12-31T23:59:59.000Z");
    }
}
