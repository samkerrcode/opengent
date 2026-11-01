//! Agent schedules: when to run an agent with a preset prompt, and what to do about runs
//! that were due while the app was closed or the computer was asleep.
//!
//! All times are epoch milliseconds. Weekly recurrences are evaluated in a caller-supplied
//! time zone (the OS local zone in the app, fixed offsets in tests), so "every weekday at
//! 09:00" stays at 09:00 local across DST changes.

use chrono::{Datelike, Days, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::domain::{AgentId, ChatId, ScheduleId};
use crate::{Error, Result};

/// A run counts as *missed* when the scheduler first sees it more than this long after it
/// was due — i.e. the app was not running or the machine was asleep at the time.
pub const MISSED_GRACE_MS: i64 = 2 * 60 * 1000;
/// Longest allowed interval: 31 days.
const MAX_INTERVAL_MINUTES: u32 = 31 * 24 * 60;
pub const MAX_SCHEDULE_PROMPT: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum Recurrence {
    /// A single run at `at`.
    Once {
        #[ts(type = "number")]
        at: i64,
    },
    /// Every `minutes`, counted from when the schedule was saved or last ran.
    Interval { minutes: u32 },
    /// On the given weekdays (0 = Monday … 6 = Sunday) at a local time of day.
    Weekly { days: Vec<u8>, minute_of_day: u16 },
}

/// What to do when the app comes back after a scheduled time passed unattended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MissedRunPolicy {
    /// Run once as soon as possible (several missed occurrences coalesce into one run).
    #[default]
    RunOnce,
    /// Skip it and wait for the next scheduled time.
    Skip,
}

/// How the most recent due time was handled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum ScheduleOutcome {
    /// Started at its scheduled time.
    Ran,
    /// Started manually ("Run now").
    Manual,
    /// Was missed and ran once when the app came back.
    CaughtUp {
        #[ts(type = "number")]
        missed_at: i64,
    },
    /// Was missed and skipped per the schedule's policy.
    Skipped {
        #[ts(type = "number")]
        missed_at: i64,
    },
    /// Could not start (e.g. the agent's model provider was removed).
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentSchedule {
    pub id: ScheduleId,
    pub agent_id: AgentId,
    /// Sent to the agent as the user message of each run.
    pub prompt: String,
    pub recurrence: Recurrence,
    pub missed_policy: MissedRunPolicy,
    pub enabled: bool,
    /// Next due time; `None` once a one-off schedule has fired.
    #[ts(type = "number | null")]
    pub next_run_at: Option<i64>,
    #[ts(type = "number | null")]
    pub last_run_at: Option<i64>,
    pub last_outcome: Option<ScheduleOutcome>,
    /// Chat created by the most recent run.
    pub last_chat_id: Option<ChatId>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleInput {
    pub id: Option<ScheduleId>,
    pub agent_id: AgentId,
    pub prompt: String,
    pub recurrence: Recurrence,
    #[serde(default)]
    pub missed_policy: MissedRunPolicy,
    pub enabled: bool,
}

impl Recurrence {
    pub fn validate(&self) -> Result<()> {
        match self {
            Recurrence::Once { .. } => Ok(()),
            Recurrence::Interval { minutes } => {
                if *minutes == 0 || *minutes > MAX_INTERVAL_MINUTES {
                    return Err(Error::InvalidInput(
                        "interval must be between 1 minute and 31 days".into(),
                    ));
                }
                Ok(())
            }
            Recurrence::Weekly {
                days,
                minute_of_day,
            } => {
                if days.is_empty() || days.iter().any(|d| *d > 6) {
                    return Err(Error::InvalidInput("choose at least one weekday".into()));
                }
                if *minute_of_day >= 24 * 60 {
                    return Err(Error::InvalidInput("time of day is invalid".into()));
                }
                Ok(())
            }
        }
    }
}

impl ScheduleInput {
    pub(crate) fn validate(&self) -> Result<String> {
        let prompt = self.prompt.trim();
        if prompt.is_empty() {
            return Err(Error::InvalidInput("the scheduled prompt is empty".into()));
        }
        if prompt.chars().count() > MAX_SCHEDULE_PROMPT {
            return Err(Error::InvalidInput(format!(
                "the scheduled prompt must be at most {MAX_SCHEDULE_PROMPT} characters"
            )));
        }
        self.recurrence.validate()?;
        Ok(prompt.to_owned())
    }
}

/// The first due time strictly after `after_ms`, or `None` when the recurrence has no
/// further occurrences (a one-off in the past).
pub fn next_after<Tz: TimeZone>(rec: &Recurrence, after_ms: i64, tz: &Tz) -> Option<i64> {
    match rec {
        Recurrence::Once { at } => (*at > after_ms).then_some(*at),
        Recurrence::Interval { minutes } => Some(after_ms + i64::from((*minutes).max(1)) * 60_000),
        Recurrence::Weekly {
            days,
            minute_of_day,
        } => {
            let after = tz.timestamp_millis_opt(after_ms).single()?;
            let time =
                NaiveTime::from_num_seconds_from_midnight_opt(u32::from(*minute_of_day) * 60, 0)?;
            let start = after.date_naive();
            // 0..=7 covers "later today" through "same weekday next week".
            for offset in 0..=7 {
                let date = start.checked_add_days(Days::new(offset))?;
                if !days.contains(&(date.weekday().num_days_from_monday() as u8)) {
                    continue;
                }
                let naive = date.and_time(time);
                // A time inside a DST gap doesn't exist; run an hour later that day instead.
                let local = tz.from_local_datetime(&naive).earliest().or_else(|| {
                    tz.from_local_datetime(&(naive + chrono::Duration::hours(1)))
                        .earliest()
                })?;
                let ms = local.timestamp_millis();
                if ms > after_ms {
                    return Some(ms);
                }
            }
            None
        }
    }
}

/// What the scheduler should do with a schedule at `now`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuePlan {
    /// Start a run now.
    pub run: bool,
    /// The due time passed unattended (app closed / asleep) — `due_at` is when.
    pub missed: bool,
    pub due_at: i64,
    /// The following due time to store (`None` = no more runs).
    pub next_run_at: Option<i64>,
}

impl DuePlan {
    pub fn outcome(&self) -> ScheduleOutcome {
        match (self.missed, self.run) {
            (false, _) => ScheduleOutcome::Ran,
            (true, true) => ScheduleOutcome::CaughtUp {
                missed_at: self.due_at,
            },
            (true, false) => ScheduleOutcome::Skipped {
                missed_at: self.due_at,
            },
        }
    }
}

/// `None` when the schedule is disabled or not yet due. Missed occurrences coalesce: the
/// next due time is always computed from `now`, so a week offline yields at most one
/// catch-up run, never a burst.
pub fn plan_due<Tz: TimeZone>(s: &AgentSchedule, now: i64, tz: &Tz) -> Option<DuePlan> {
    let due_at = s.next_run_at?;
    if !s.enabled || due_at > now {
        return None;
    }
    let missed = now - due_at > MISSED_GRACE_MS;
    Some(DuePlan {
        run: !missed || s.missed_policy == MissedRunPolicy::RunOnce,
        missed,
        due_at,
        next_run_at: next_after(&s.recurrence, now, tz),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Utc};

    fn ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp_millis()
    }

    fn schedule(rec: Recurrence, next: i64, policy: MissedRunPolicy) -> AgentSchedule {
        AgentSchedule {
            id: ScheduleId::new(),
            agent_id: AgentId::new(),
            prompt: "p".into(),
            recurrence: rec,
            missed_policy: policy,
            enabled: true,
            next_run_at: Some(next),
            last_run_at: None,
            last_outcome: None,
            last_chat_id: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn weekly_finds_next_matching_day_in_local_time() {
        // 2026-11-02 is a Monday. 09:00 at UTC+2 is 07:00 UTC.
        let tz = FixedOffset::east_opt(2 * 3600).unwrap();
        let weekdays = Recurrence::Weekly {
            days: vec![0, 1, 2, 3, 4],
            minute_of_day: 9 * 60,
        };
        // Monday 06:00 UTC (08:00 local) → same day 07:00 UTC.
        assert_eq!(
            next_after(&weekdays, ms(2026, 11, 2, 6, 0), &tz),
            Some(ms(2026, 11, 2, 7, 0))
        );
        // Exactly at the due time → strictly after, so Tuesday.
        assert_eq!(
            next_after(&weekdays, ms(2026, 11, 2, 7, 0), &tz),
            Some(ms(2026, 11, 3, 7, 0))
        );
        // Friday after 09:00 local → Monday.
        assert_eq!(
            next_after(&weekdays, ms(2026, 11, 6, 8, 0), &tz),
            Some(ms(2026, 11, 9, 7, 0))
        );
        // Single weekday, already passed today → one week later.
        let mondays = Recurrence::Weekly {
            days: vec![0],
            minute_of_day: 9 * 60,
        };
        assert_eq!(
            next_after(&mondays, ms(2026, 11, 2, 8, 0), &tz),
            Some(ms(2026, 11, 9, 7, 0))
        );
    }

    #[test]
    fn once_and_interval() {
        let at = ms(2026, 11, 2, 12, 0);
        assert_eq!(next_after(&Recurrence::Once { at }, at - 1, &Utc), Some(at));
        assert_eq!(next_after(&Recurrence::Once { at }, at, &Utc), None);
        assert_eq!(
            next_after(&Recurrence::Interval { minutes: 30 }, at, &Utc),
            Some(at + 30 * 60_000)
        );
    }

    #[test]
    fn on_time_runs_and_missed_runs_follow_policy() {
        let due = ms(2026, 11, 2, 9, 0);
        let rec = Recurrence::Interval { minutes: 60 };

        // Seen within the grace window: a normal run.
        let s = schedule(rec.clone(), due, MissedRunPolicy::Skip);
        let p = plan_due(&s, due + 30_000, &Utc).unwrap();
        assert!(p.run && !p.missed);
        assert_eq!(p.outcome(), ScheduleOutcome::Ran);

        // App was closed for 5 hours: skip → no run, next time computed from now.
        let now = due + 5 * 3_600_000;
        let p = plan_due(&s, now, &Utc).unwrap();
        assert!(!p.run && p.missed);
        assert_eq!(p.next_run_at, Some(now + 3_600_000));
        assert_eq!(p.outcome(), ScheduleOutcome::Skipped { missed_at: due });

        // Same gap with run-once: exactly one catch-up run.
        let s = schedule(rec, due, MissedRunPolicy::RunOnce);
        let p = plan_due(&s, now, &Utc).unwrap();
        assert!(p.run && p.missed);
        assert_eq!(p.outcome(), ScheduleOutcome::CaughtUp { missed_at: due });
    }

    #[test]
    fn not_due_disabled_and_finished_schedules_are_ignored() {
        let due = ms(2026, 11, 2, 9, 0);
        let mut s = schedule(Recurrence::Once { at: due }, due, MissedRunPolicy::RunOnce);
        assert!(plan_due(&s, due - 1, &Utc).is_none());
        let p = plan_due(&s, due, &Utc).unwrap();
        assert_eq!(p.next_run_at, None, "one-off schedules end after firing");
        s.enabled = false;
        assert!(plan_due(&s, due, &Utc).is_none());
        s.enabled = true;
        s.next_run_at = None;
        assert!(plan_due(&s, due, &Utc).is_none());
    }

    #[test]
    fn recurrence_validation() {
        assert!(Recurrence::Interval { minutes: 0 }.validate().is_err());
        assert!(Recurrence::Weekly {
            days: vec![],
            minute_of_day: 0
        }
        .validate()
        .is_err());
        assert!(Recurrence::Weekly {
            days: vec![7],
            minute_of_day: 0
        }
        .validate()
        .is_err());
        assert!(Recurrence::Weekly {
            days: vec![6],
            minute_of_day: 1440
        }
        .validate()
        .is_err());
        assert!(Recurrence::Weekly {
            days: vec![6],
            minute_of_day: 1439
        }
        .validate()
        .is_ok());
    }
}
