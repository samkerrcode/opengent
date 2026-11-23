//! `schedule add | list | remove`. Schedules are rows in the shared database; the desktop
//! app's scheduler fires them while it is open. There is no headless scheduler.

use agent_core::agents::{Agent, AgentSchedule, MissedRunPolicy, Recurrence, ScheduleInput};
use agent_core::config::resolve_data_root;
use agent_core::domain::ScheduleId;
use agent_core::storage::agents as repo;
use agent_core::storage::Db;
use chrono::{Local, NaiveDateTime, NaiveTime, TimeZone};

use std::fmt::Write as _;

use crate::args::{self, Arg, Args};
use crate::{no_more, term, CliError, Result};

pub fn schedule(args: &mut Args) -> Result<()> {
    match args.word().as_deref() {
        Some("add") => add(args),
        Some("list") => list(args),
        Some("remove" | "rm") => remove(args),
        Some(other) => Err(CliError::Usage(format!(
            "unknown command `schedule {other}`"
        ))),
        None => Err(CliError::Usage(
            "`schedule` needs a command: add, list or remove".into(),
        )),
    }
}

fn open_db() -> Result<(Db, agent_core::storage::ProcessLock)> {
    let root = resolve_data_root(None)?;
    Ok(Db::open_root(&root)?)
}

/// An agent by id, or by name (case-insensitive) when that is unambiguous.
pub fn find_agent(agents: &[Agent], spec: &str) -> Result<Agent> {
    let spec = spec.trim();
    if let Some(a) = agents.iter().find(|a| a.id.to_string() == spec) {
        return Ok(a.clone());
    }
    let named: Vec<&Agent> = agents
        .iter()
        .filter(|a| a.name.eq_ignore_ascii_case(spec))
        .collect();
    match named.as_slice() {
        [a] => Ok((*a).clone()),
        [] => Err(CliError::Usage(format!("no agent named `{spec}`"))),
        _ => Err(CliError::Usage(format!(
            "several agents are named `{spec}`; use its id: {}",
            named
                .iter()
                .map(|a| a.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn add(args: &mut Args) -> Result<()> {
    let (mut agent, mut at, mut every, mut skip) = (None, None, None, false);
    let mut words = Vec::new();
    while let Some(a) = args.next_arg() {
        match a {
            Arg::Flag(f, v) => match f.as_str() {
                "agent" => agent = Some(args.value(&f, v)?),
                "at" => at = Some(args.value(&f, v)?),
                "every" => every = Some(args.value(&f, v)?),
                "skip-missed" => {
                    args::switch(&f, v)?;
                    skip = true;
                }
                _ => return Err(args::unknown(&f)),
            },
            Arg::Positional(p) => words.push(p),
        }
    }
    let agent = agent.ok_or_else(|| CliError::Usage("--agent is required".into()))?;
    let now = chrono::Utc::now().timestamp_millis();
    let recurrence = match (at, every) {
        (Some(at), None) => Recurrence::Once {
            at: parse_at(&at, now, &Local)?,
        },
        (None, Some(every)) => parse_every(&every)?,
        _ => {
            return Err(CliError::Usage(
                "give exactly one of --at or --every".into(),
            ))
        }
    };
    let prompt = words.join(" ");
    let (db, _lock) = open_db()?;
    let schedule = db.call_blocking(move |c| {
        let agent = find_agent(&repo::list_agents(c)?, &agent)
            .map_err(|e| agent_core::Error::InvalidInput(message(e)))?;
        let s = repo::save_schedule(
            c,
            ScheduleInput {
                id: None,
                agent_id: agent.id,
                prompt,
                recurrence,
                missed_policy: if skip {
                    MissedRunPolicy::Skip
                } else {
                    MissedRunPolicy::RunOnce
                },
                enabled: true,
            },
            now,
            &Local,
        )?;
        Ok((s, agent.name))
    })?;
    let (s, name) = schedule;
    term::out(&format!("{}\n", s.id))?;
    term::note(&format!(
        "Scheduled agent \"{name}\" {}; next run {}.\nSchedules run while the desktop app is \
         open; one that comes due while it is closed is {} when it opens.",
        describe(&s.recurrence),
        s.next_run_at
            .map(local_time)
            .unwrap_or_else(|| "none".into()),
        if skip { "skipped" } else { "run once" }
    ));
    Ok(())
}

fn message(e: CliError) -> String {
    match e {
        CliError::Usage(m) | CliError::Failed(m) => m,
        CliError::Exit(c) => format!("exit {c}"),
    }
}

fn list(args: &mut Args) -> Result<()> {
    let (mut agent, mut json) = (None, false);
    while let Some(a) = args.next_arg() {
        match a {
            Arg::Flag(f, v) if f == "agent" => agent = Some(args.value(&f, v)?),
            Arg::Flag(f, v) if f == "json" => {
                args::switch(&f, v)?;
                json = true;
            }
            Arg::Flag(f, _) => return Err(args::unknown(&f)),
            Arg::Positional(p) => {
                return Err(CliError::Usage(format!("unexpected argument `{p}`")))
            }
        }
    }
    let (db, _lock) = open_db()?;
    let (agents, schedules): (Vec<Agent>, Vec<AgentSchedule>) =
        db.call_blocking(|c| Ok((repo::list_agents(c)?, repo::list_schedules(c, None)?)))?;
    let filter = agent.map(|a| find_agent(&agents, &a)).transpose()?;
    let schedules: Vec<AgentSchedule> = schedules
        .into_iter()
        .filter(|s| filter.as_ref().is_none_or(|a| a.id == s.agent_id))
        .collect();
    if json {
        return crate::list::print_json(&schedules);
    }
    if schedules.is_empty() {
        return term::out("No schedules.\n");
    }
    let mut text = String::new();
    for s in &schedules {
        let name = agents
            .iter()
            .find(|a| a.id == s.agent_id)
            .map(|a| term::sanitize(&a.name, false))
            .unwrap_or_default();
        let _ = writeln!(
            text,
            "{}  {name}  {}  next {}{}",
            s.id,
            describe(&s.recurrence),
            s.next_run_at.map(local_time).unwrap_or_else(|| "—".into()),
            if s.enabled { "" } else { "  (off)" }
        );
    }
    term::out(&text)
}

fn remove(args: &mut Args) -> Result<()> {
    let id = match args.next_arg() {
        Some(Arg::Positional(p)) => p,
        Some(Arg::Flag(f, _)) => return Err(args::unknown(&f)),
        None => return Err(CliError::Usage("give the schedule id to remove".into())),
    };
    no_more(args)?;
    let id: ScheduleId = id
        .parse()
        .map_err(|_| CliError::Usage(format!("`{id}` is not a schedule id")))?;
    let (db, _lock) = open_db()?;
    db.call_blocking(move |c| repo::delete_schedule(c, id))?;
    term::note(&format!("Removed schedule {id}."));
    Ok(())
}

fn local_time(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

fn describe(r: &Recurrence) -> String {
    match r {
        Recurrence::Once { at } => format!("once at {}", local_time(*at)),
        Recurrence::Interval { minutes } => match minutes {
            m if m % (24 * 60) == 0 => format!("every {} d", m / (24 * 60)),
            m if m % 60 == 0 => format!("every {} h", m / 60),
            m => format!("every {m} min"),
        },
        Recurrence::Weekly {
            days,
            minute_of_day,
        } => format!(
            "{} at {:02}:{:02}",
            days.iter()
                .filter_map(|d| DAYS.get(*d as usize).copied())
                .collect::<Vec<_>>()
                .join(","),
            minute_of_day / 60,
            minute_of_day % 60
        ),
    }
}

/// `YYYY-MM-DD HH:MM[:SS]` (or with `T`) in local time, or `HH:MM` for its next
/// occurrence. Must be in the future.
fn parse_at<Tz: TimeZone>(raw: &str, now: i64, tz: &Tz) -> Result<i64> {
    let raw = raw.trim();
    let bad = || {
        CliError::Usage(format!(
            "can't read the time `{raw}` — use \"YYYY-MM-DD HH:MM\" or \"HH:MM\""
        ))
    };
    let local = |naive: NaiveDateTime| {
        tz.from_local_datetime(&naive)
            .earliest()
            .map(|t| t.timestamp_millis())
            .ok_or_else(bad)
    };
    let full = [
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
    ]
    .iter()
    .find_map(|f| NaiveDateTime::parse_from_str(raw, f).ok());
    let at = match full {
        Some(naive) => local(naive)?,
        None => {
            let time = NaiveTime::parse_from_str(raw, "%H:%M").map_err(|_| bad())?;
            let today = tz
                .timestamp_millis_opt(now)
                .single()
                .ok_or_else(bad)?
                .date_naive();
            let at = local(today.and_time(time))?;
            if at > now {
                at
            } else {
                local(today.succ_opt().ok_or_else(bad)?.and_time(time))?
            }
        }
    };
    if at <= now {
        return Err(CliError::Usage(format!("`{raw}` is in the past")));
    }
    Ok(at)
}

/// `30m`, `2h`, `1d` (interval) or `<days>@HH:MM` with `daily`, `weekdays`, `weekends` or a
/// comma list of `mon`…`sun`.
fn parse_every(raw: &str) -> Result<Recurrence> {
    let raw = raw.trim().to_ascii_lowercase();
    let bad = || {
        CliError::Usage(format!(
            "can't read `--every {raw}` — use e.g. 30m, 2h, 1d, weekdays@09:00 or mon,thu@18:30"
        ))
    };
    if let Some((days, time)) = raw.split_once('@') {
        let time = NaiveTime::parse_from_str(time.trim(), "%H:%M").map_err(|_| bad())?;
        let days: Vec<u8> = match days.trim() {
            "daily" => (0..7).collect(),
            "weekdays" => (0..5).collect(),
            "weekends" => vec![5, 6],
            list => list
                .split(',')
                .map(|d| {
                    DAYS.iter()
                        .position(|n| *n == d.trim().get(..3).unwrap_or(""))
                        .map(|i| i as u8)
                        .ok_or_else(bad)
                })
                .collect::<Result<_>>()?,
        };
        use chrono::Timelike;
        let r = Recurrence::Weekly {
            days,
            minute_of_day: (time.hour() * 60 + time.minute()) as u16,
        };
        r.validate()?;
        return Ok(r);
    }
    let split = raw.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?;
    let n: u32 = raw[..split].parse().map_err(|_| bad())?;
    let minutes = match &raw[split..] {
        "m" | "min" => n,
        "h" => n.checked_mul(60).ok_or_else(bad)?,
        "d" => n.checked_mul(24 * 60).ok_or_else(bad)?,
        _ => return Err(bad()),
    };
    let r = Recurrence::Interval { minutes };
    r.validate()?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn every_parses_intervals_and_weekly_times() {
        assert_eq!(
            parse_every("90m").unwrap(),
            Recurrence::Interval { minutes: 90 }
        );
        assert_eq!(
            parse_every("2h").unwrap(),
            Recurrence::Interval { minutes: 120 }
        );
        assert_eq!(
            parse_every("weekdays@09:30").unwrap(),
            Recurrence::Weekly {
                days: vec![0, 1, 2, 3, 4],
                minute_of_day: 570
            }
        );
        assert_eq!(
            parse_every("Mon,Thursday@18:00").unwrap(),
            Recurrence::Weekly {
                days: vec![0, 3],
                minute_of_day: 1080
            }
        );
        for bad in ["", "m", "0m", "40d", "5w", "funday@09:00", "daily@25:00"] {
            assert!(parse_every(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn at_reads_local_times_and_rejects_the_past() {
        let tz = FixedOffset::east_opt(3600).unwrap();
        // 2026-01-10 12:00 at +01:00.
        let now = tz
            .with_ymd_and_hms(2026, 1, 10, 12, 0, 0)
            .unwrap()
            .timestamp_millis();
        let at = |h, m, d| {
            tz.with_ymd_and_hms(2026, 1, d, h, m, 0)
                .unwrap()
                .timestamp_millis()
        };
        assert_eq!(
            parse_at("2026-01-11 08:15", now, &tz).unwrap(),
            at(8, 15, 11)
        );
        assert_eq!(
            parse_at("2026-01-11T08:15", now, &tz).unwrap(),
            at(8, 15, 11)
        );
        assert_eq!(parse_at("13:00", now, &tz).unwrap(), at(13, 0, 10));
        assert_eq!(
            parse_at("11:00", now, &tz).unwrap(),
            at(11, 0, 11),
            "tomorrow"
        );
        assert!(parse_at("2026-01-09 08:00", now, &tz).is_err());
        assert!(parse_at("soon", now, &tz).is_err());
    }
}
