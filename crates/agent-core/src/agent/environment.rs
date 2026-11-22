//! Runtime context for models: the host OS and time zone, and the local send time of each
//! user message. Split by how often each part changes, so the request prefix stays
//! cacheable: the OS and zone go into the system prompt (`## Environment`), the clock goes
//! onto user messages as a stamp derived from their stored `created_at`.
//!
//! Collected here, not by a tool, so it works with native tools off and in scheduled runs.
//! Nothing identifying is collected (no hostname, username or paths); a lookup that fails
//! leaves its field out and never fails the run.

use std::fmt::Display;

use chrono::{Offset, TimeZone};

/// The stable part of the snapshot: changes only on an OS update or a time zone change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// OS name and version, e.g. `macOS 12.6`.
    pub os: Option<String>,
    /// IANA zone name, or the current UTC offset (`UTC-05:00`) when the name is unknown.
    pub time_zone: Option<String>,
}

impl Environment {
    /// Looks up the host OS and local time zone.
    pub fn detect() -> Self {
        let (os, zone) = platform::lookup();
        Self {
            os,
            time_zone: zone.filter(|z| is_zone_name(z)).or_else(|| {
                Some(utc_offset(
                    chrono::Local::now().offset().fix().local_minus_utc(),
                ))
            }),
        }
    }

    /// The `## Environment` system prompt section.
    pub fn prompt(&self) -> String {
        let fields: Vec<&str> = [self.os.as_deref(), self.time_zone.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let mut s = String::from("\n\n## Environment\n");
        if !fields.is_empty() {
            s.push_str(&fields.join(", "));
            s.push_str(". ");
        }
        s.push_str("User messages start with their local send time.");
        s
    }
}

/// Environment inputs to context compilation. The default (sharing off) adds nothing, so
/// the request is exactly what it would be without this feature.
#[derive(Debug, Clone, Default)]
pub struct EnvContext {
    /// Appended to the system prompt after the tools section (see `Environment::prompt`).
    pub prompt: String,
    /// Send-time stamp per history message, by index; applied to user messages only.
    pub stamps: Vec<Option<String>>,
}

impl EnvContext {
    /// `sent_at` holds each history message's `created_at` (ms), in history order.
    pub fn new<Tz: TimeZone>(
        env: &Environment,
        sent_at: impl IntoIterator<Item = i64>,
        tz: &Tz,
    ) -> Self
    where
        Tz::Offset: Display,
    {
        Self {
            prompt: env.prompt(),
            stamps: sent_at.into_iter().map(|ms| send_stamp(ms, tz)).collect(),
        }
    }
}

/// `[Sun 2026-11-22 14:35]`: the instant `ms` in `tz`, with the zone rules in effect at that
/// instant, so a later DST change does not rewrite it.
pub fn send_stamp<Tz: TimeZone>(ms: i64, tz: &Tz) -> Option<String>
where
    Tz::Offset: Display,
{
    tz.timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("[%a %Y-%m-%d %H:%M]").to_string())
}

fn utc_offset(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let mins = secs.unsigned_abs() / 60;
    format!("UTC{sign}{:02}:{:02}", mins / 60, mins % 60)
}

/// IANA-shaped (`America/Argentina/Buenos_Aires`, `Etc/GMT+5`, `UTC`), so nothing else from
/// the environment (a POSIX `TZ` rule, a stray path) reaches the prompt.
fn is_zone_name(z: &str) -> bool {
    (1..=64).contains(&z.len())
        && z.starts_with(|c: char| c.is_ascii_alphabetic())
        && !z.contains("..")
        && z.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
}

/// `12.6.3` → `12.6`; `None` unless it is a dotted number.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn major_minor(version: &str) -> Option<String> {
    let parts: Vec<&str> = version.trim().split('.').take(2).collect();
    parts
        .iter()
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        .then(|| parts.join("."))
}

/// `DeviceFamilyVersion` (major, minor, build, revision packed into a `u64`) → `Windows 11
/// (build 22631)`. Windows 11 still reports 10.0; the build number tells them apart.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_name(device_family_version: &str) -> Option<String> {
    let v: u64 = device_family_version.trim().parse().ok()?;
    let (major, minor, build) = (v >> 48, (v >> 32) & 0xFFFF, (v >> 16) & 0xFFFF);
    let name = match (major, minor) {
        (10, 0) if build >= 22_000 => "Windows 11".to_owned(),
        (10, 0) => "Windows 10".to_owned(),
        _ => format!("Windows {major}.{minor}"),
    };
    Some(format!("{name} (build {build})"))
}

/// The zone name from a `TZ` value or `/etc/localtime` link target: the part after
/// `zoneinfo/`, or the value itself.
#[cfg_attr(not(unix), allow(dead_code))]
fn zone_from_path(path: &str) -> &str {
    let path = path.trim().trim_start_matches(':');
    path.rsplit_once("zoneinfo/").map_or(path, |(_, name)| name)
}

#[cfg(unix)]
fn unix_zone() -> Option<String> {
    // `TZ` overrides the system zone (chrono's `Local` honors it too).
    if let Ok(tz) = std::env::var("TZ") {
        return Some(zone_from_path(&tz).to_owned()).filter(|z| !z.is_empty());
    }
    let link = std::fs::read_link("/etc/localtime").ok();
    match link.as_deref().and_then(std::path::Path::to_str) {
        Some(target) if target.contains("zoneinfo/") => Some(zone_from_path(target).to_owned()),
        _ => std::fs::read_to_string("/etc/timezone")
            .ok()
            .map(|z| z.trim().to_owned()),
    }
}

#[cfg(target_os = "macos")]
mod platform {
    pub fn lookup() -> (Option<String>, Option<String>) {
        (os(), super::unix_zone())
    }

    fn os() -> Option<String> {
        let plist =
            std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist").ok()?;
        let rest = &plist[plist.find("<key>ProductVersion</key>")?..];
        let start = rest.find("<string>")? + "<string>".len();
        let end = start + rest[start..].find("</string>")?;
        super::major_minor(&rest[start..end]).map(|v| format!("macOS {v}"))
    }
}

#[cfg(windows)]
mod platform {
    use windows::Globalization::Calendar;
    use windows::System::Profile::AnalyticsInfo;
    use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

    /// WinRT needs an apartment: a short-lived thread joins the multithreaded one, leaving
    /// the caller's thread as it was.
    pub fn lookup() -> (Option<String>, Option<String>) {
        std::thread::spawn(|| {
            // SAFETY: paired with `RoUninitialize` below, on this thread.
            if unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_err() {
                return (None, None);
            }
            let os = AnalyticsInfo::VersionInfo()
                .and_then(|v| v.DeviceFamilyVersion())
                .ok()
                .and_then(|v| super::windows_name(&v.to_string()));
            // The IANA id (`America/Toronto`), not the Windows zone key.
            let zone = Calendar::new()
                .and_then(|c| c.GetTimeZone())
                .ok()
                .map(|z| z.to_string());
            // SAFETY: balances the successful `RoInitialize`; the WinRT objects are dropped.
            unsafe { RoUninitialize() };
            (os, zone)
        })
        .join()
        .unwrap_or_default()
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    pub fn lookup() -> (Option<String>, Option<String>) {
        #[cfg(unix)]
        let zone = super::unix_zone();
        #[cfg(not(unix))]
        let zone = None;
        (os(), zone)
    }

    /// `NAME VERSION_ID` from `/etc/os-release`, e.g. `Ubuntu 24.04`.
    fn os() -> Option<String> {
        let release = std::fs::read_to_string("/etc/os-release").ok()?;
        let field = |key: &str| {
            release.lines().find_map(|l| {
                l.strip_prefix(key)?
                    .strip_prefix('=')
                    .map(|v| v.trim().trim_matches('"').to_owned())
            })
        };
        let name = field("NAME").filter(|n| !n.is_empty())?;
        Some(match field("VERSION_ID") {
            Some(v) if !v.is_empty() => format!("{name} {v}"),
            _ => name,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::{FixedOffset, MappedLocalTime, NaiveDate, NaiveDateTime};

    /// Toronto around the 2026 fall-back: EDT (UTC−4) until 2026-11-01 06:00 UTC, EST
    /// (UTC−5) from then on.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct Toronto;

    pub(crate) const FALL_BACK_MS: i64 = 1_793_512_800_000;

    impl Toronto {
        fn at(ms: i64) -> FixedOffset {
            let hours = if ms < FALL_BACK_MS { -4 } else { -5 };
            FixedOffset::east_opt(hours * 3600).unwrap()
        }
    }

    impl TimeZone for Toronto {
        type Offset = FixedOffset;
        fn from_offset(_: &FixedOffset) -> Self {
            Self
        }
        fn offset_from_local_date(&self, _: &NaiveDate) -> MappedLocalTime<FixedOffset> {
            MappedLocalTime::None
        }
        fn offset_from_local_datetime(&self, _: &NaiveDateTime) -> MappedLocalTime<FixedOffset> {
            MappedLocalTime::None
        }
        fn offset_from_utc_date(&self, d: &NaiveDate) -> FixedOffset {
            self.offset_from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
        }
        fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> FixedOffset {
            Self::at(utc.and_utc().timestamp_millis())
        }
    }

    fn env(os: Option<&str>, zone: Option<&str>) -> Environment {
        Environment {
            os: os.map(Into::into),
            time_zone: zone.map(Into::into),
        }
    }

    #[test]
    fn prompt_is_one_line_and_omits_missing_fields() {
        assert_eq!(
            env(Some("macOS 12.6"), Some("America/Toronto")).prompt(),
            "\n\n## Environment\nmacOS 12.6, America/Toronto. User messages start with their \
             local send time."
        );
        assert_eq!(
            env(None, Some("UTC-05:00")).prompt(),
            "\n\n## Environment\nUTC-05:00. User messages start with their local send time."
        );
        assert_eq!(
            env(Some("Windows 11 (build 22631)"), None).prompt(),
            "\n\n## Environment\nWindows 11 (build 22631). User messages start with their \
             local send time."
        );
        assert_eq!(
            env(None, None).prompt(),
            "\n\n## Environment\nUser messages start with their local send time."
        );
    }

    #[test]
    fn stamp_has_weekday_and_minute_precision() {
        // 2026-11-22 19:35:59 UTC = 14:35 EST, a Sunday.
        assert_eq!(
            send_stamp(1_795_376_159_000, &Toronto).as_deref(),
            Some("[Sun 2026-11-22 14:35]")
        );
    }

    #[test]
    fn stamps_use_the_rules_in_effect_at_each_instant() {
        // One minute before the fall-back (01:59 EDT) and one hour after it (01:59 EST).
        let before = FALL_BACK_MS - 60_000;
        let after = FALL_BACK_MS + 3_540_000;
        let early = EnvContext::new(&Environment::default(), [before], &Toronto);
        let late = EnvContext::new(&Environment::default(), [before, after], &Toronto);
        assert_eq!(early.stamps[0].as_deref(), Some("[Sun 2026-11-01 01:59]"));
        assert_eq!(late.stamps[0], early.stamps[0]);
        assert_eq!(late.stamps[1].as_deref(), Some("[Sun 2026-11-01 01:59]"));
    }

    #[test]
    fn offsets_and_zone_names() {
        assert_eq!(utc_offset(-5 * 3600), "UTC-05:00");
        assert_eq!(utc_offset(5 * 3600 + 1800), "UTC+05:30");
        assert_eq!(utc_offset(0), "UTC+00:00");
        for ok in [
            "America/Toronto",
            "Etc/GMT+5",
            "UTC",
            "America/Port-au-Prince",
        ] {
            assert!(is_zone_name(ok), "{ok}");
        }
        for bad in [
            "",
            "EST5EDT,M3.2.0,M11.1.0",
            "<+03>-3",
            "/etc/x",
            "a/../b",
            "A b",
        ] {
            assert!(!is_zone_name(bad), "{bad}");
        }
        assert_eq!(
            zone_from_path("/var/db/timezone/zoneinfo/America/Toronto"),
            "America/Toronto"
        );
        assert_eq!(
            zone_from_path(":/usr/share/zoneinfo/Europe/Paris"),
            "Europe/Paris"
        );
        assert_eq!(zone_from_path("Asia/Tokyo"), "Asia/Tokyo");
    }

    #[test]
    fn os_versions() {
        assert_eq!(major_minor("12.6.3").as_deref(), Some("12.6"));
        assert_eq!(major_minor("14.0").as_deref(), Some("14.0"));
        assert_eq!(major_minor("15").as_deref(), Some("15"));
        assert_eq!(major_minor("12.x"), None);
        let packed = |major: u64, minor: u64, build: u64| {
            ((major << 48) | (minor << 32) | (build << 16) | 1).to_string()
        };
        assert_eq!(
            windows_name(&packed(10, 0, 22631)).as_deref(),
            Some("Windows 11 (build 22631)")
        );
        assert_eq!(
            windows_name(&packed(10, 0, 19045)).as_deref(),
            Some("Windows 10 (build 19045)")
        );
        assert_eq!(windows_name("nope"), None);
    }

    #[test]
    fn detect_never_fails_and_always_has_a_zone() {
        let env = Environment::detect();
        assert!(env.time_zone.is_some());
        #[cfg(any(target_os = "macos", windows))]
        assert!(env.os.is_some(), "{env:?}");
    }
}
