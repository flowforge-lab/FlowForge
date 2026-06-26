//! Cron parsing and derivation — the single source of truth for `next_run`,
//! the due-detection `prev_occurrence`, and the human `cadence_label` (RFC 0017
//! §4–§5). Expressions are 6-field (`sec min hour dom mon dow`, the form the
//! `cron` crate parses) and evaluated in the host's **local** timezone, because
//! "Daily at 9:00 AM" means the user's wall-clock time, not UTC.

use std::str::FromStr;

#[cfg(test)]
use chrono::TimeZone;
use chrono::{DateTime, Local, Timelike, Weekday};
use cron::Schedule;

/// Parse and validate a cron expression. Used by `create`/`preview` to reject a
/// bad expression before it is stored.
pub fn parse(expr: &str) -> Result<Schedule, String> {
    Schedule::from_str(expr).map_err(|e| format!("invalid cron expression: {e}"))
}

/// Convert a local `DateTime` to epoch milliseconds (the wire/storage unit).
fn to_ms(dt: DateTime<Local>) -> i64 {
    dt.timestamp_millis()
}

/// The next future occurrence strictly after `now`, as epoch ms. `None` when the
/// expression has no future occurrence. Display-only — NOT the fire trigger.
pub fn next_run(schedule: &Schedule, now: DateTime<Local>) -> Option<i64> {
    schedule.after(&now).next().map(to_ms)
}

/// The most recent occurrence at or before `now`, as epoch ms — the primitive
/// the runner's due predicate keys off (`prev_occurrence > last_run`). `None`
/// when the expression has no past occurrence (a brand-new far-future task).
///
/// `cron`'s `after()` iterator is double-ended, so the previous occurrence is the
/// last element strictly before `now`; we step back from `now` minus one second
/// so an occurrence landing exactly on `now` still counts as "at or before".
pub fn prev_occurrence(schedule: &Schedule, now: DateTime<Local>) -> Option<i64> {
    let from = now + chrono::Duration::seconds(1);
    schedule
        .after(&from)
        .next_back()
        .filter(|dt| *dt <= now)
        .map(to_ms)
}

/// Derive the human cadence label (e.g. "Daily at 5:00 PM", "Weekdays at 9:00
/// AM", "Mon, Wed, Fri at 10:30 AM") from a cron expression, evaluated in local
/// time. Falls back to the raw expression for shapes we do not special-case.
pub fn cadence_label(expr: &str) -> String {
    let Ok(schedule) = parse(expr) else {
        return expr.to_string();
    };
    // Two consecutive occurrences tell us the time-of-day and the day pattern.
    let now = Local::now();
    let mut it = schedule.after(&now);
    let Some(a) = it.next() else {
        return expr.to_string();
    };
    let time = format_time(&a);

    let fields: Vec<&str> = expr.split_whitespace().collect();
    // 6-field: sec min hour dom mon dow
    if fields.len() == 6 {
        let (dom, dow) = (fields[3], fields[5]);
        // Hourly: a wildcard hour with a concrete minute (e.g. `0 30 * * * *`).
        // A wildcard minute too (`0 * * * * *`) is every-minute, not hourly, so
        // it falls through to the generic label rather than being mislabeled.
        if fields[2] == "*" && fields[1] != "*" {
            return "Hourly".to_string();
        }
        if dow == "*" && dom == "*" {
            return format!("Daily at {time}");
        }
        if dom == "*" && dow != "*" {
            return weekday_label(dow, &time);
        }
        if dom != "*" && dow == "*" {
            if let Ok(day) = dom.parse::<u32>() {
                return format!("Monthly on day {day} at {time}");
            }
        }
    }
    format!("At {time}")
}

fn format_time(dt: &DateTime<Local>) -> String {
    let (is_pm, h12) = dt.hour12();
    let min = dt.minute();
    let ampm = if is_pm { "PM" } else { "AM" };
    format!("{h12}:{min:02} {ampm}")
}

/// Render a day-of-week field as a label. Recognises the Mon–Fri set as
/// "Weekdays"; otherwise lists the abbreviated day names in week order.
fn weekday_label(dow: &str, time: &str) -> String {
    let days = parse_dow(dow);
    if days
        == [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
        ]
    {
        return format!("Weekdays at {time}");
    }
    if days.is_empty() {
        return format!("At {time}");
    }
    let names: Vec<&str> = days.iter().map(|d| weekday_name(*d)).collect();
    format!("{} at {time}", names.join(", "))
}

/// Parse a day-of-week field (`Mon,Wed,Fri`, `1,3,5`, or a range `Mon-Fri`)
/// into ordered weekdays.
fn parse_dow(dow: &str) -> Vec<Weekday> {
    let mut out: Vec<Weekday> = Vec::new();
    for tok in dow.split(',') {
        if let Some((lo, hi)) = tok.split_once('-') {
            // Range form, e.g. `Mon-Fri` or `1-5`.
            if let (Some(a), Some(b)) = (parse_dow_token(lo), parse_dow_token(hi)) {
                let (a, b) = (a.num_days_from_monday(), b.num_days_from_monday());
                for n in a..=b {
                    out.push(weekday_from_monday(n));
                }
            }
        } else if let Some(d) = parse_dow_token(tok) {
            out.push(d);
        }
    }
    out.sort_by_key(|d| d.num_days_from_monday());
    out.dedup();
    out
}

/// Parse a single day-of-week token (name or cron number; 0/7 = Sunday).
fn parse_dow_token(tok: &str) -> Option<Weekday> {
    match tok {
        "0" | "7" => Some(Weekday::Sun),
        "1" => Some(Weekday::Mon),
        "2" => Some(Weekday::Tue),
        "3" => Some(Weekday::Wed),
        "4" => Some(Weekday::Thu),
        "5" => Some(Weekday::Fri),
        "6" => Some(Weekday::Sat),
        _ => tok.parse::<Weekday>().ok(),
    }
}

fn weekday_from_monday(n: u32) -> Weekday {
    match n % 7 {
        0 => Weekday::Mon,
        1 => Weekday::Tue,
        2 => Weekday::Wed,
        3 => Weekday::Thu,
        4 => Weekday::Fri,
        5 => Weekday::Sat,
        _ => Weekday::Sun,
    }
}

fn weekday_name(d: Weekday) -> &'static str {
    match d {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
}

/// Canonical 6-field expressions for the New-task form presets (RFC 0017 §5).
/// `hour`/`minute` are local wall-clock; `dow` is a cron day-of-week field.
pub fn daily(hour: u32, minute: u32) -> String {
    format!("0 {minute} {hour} * * *")
}

pub fn weekly(hour: u32, minute: u32, dow: &str) -> String {
    format!("0 {minute} {hour} * * {dow}")
}

pub fn monthly(hour: u32, minute: u32, day: u32) -> String {
    format!("0 {minute} {hour} {day} * *")
}

pub fn hourly(minute: u32) -> String {
    format!("0 {minute} * * * *")
}

/// Build a local `DateTime` for a test, panicking on an impossible time.
#[cfg(test)]
pub(crate) fn local(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Local> {
    Local
        .with_ymd_and_hms(y, m, d, h, min, s)
        .single()
        .expect("valid local time")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_five_field_and_garbage() {
        assert!(parse("0 17 * * *").is_err(), "5-field must be rejected");
        assert!(parse("not a cron").is_err());
        assert!(parse("0 0 17 * * *").is_ok());
    }

    #[test]
    fn next_and_prev_bracket_now() {
        // 17:00 daily; now is 18:15 on 2026-06-26.
        let sched = parse("0 0 17 * * *").unwrap();
        let now = local(2026, 6, 26, 18, 15, 0);
        let next = next_run(&sched, now).unwrap();
        let prev = prev_occurrence(&sched, now).unwrap();
        // next = tomorrow 17:00, prev = today 17:00.
        assert_eq!(next, to_ms(local(2026, 6, 27, 17, 0, 0)));
        assert_eq!(prev, to_ms(local(2026, 6, 26, 17, 0, 0)));
    }

    #[test]
    fn prev_occurrence_includes_exact_now() {
        let sched = parse("0 0 17 * * *").unwrap();
        let now = local(2026, 6, 26, 17, 0, 0); // exactly on a slot
        assert_eq!(
            prev_occurrence(&sched, now).unwrap(),
            to_ms(local(2026, 6, 26, 17, 0, 0))
        );
    }

    #[test]
    fn cadence_labels_match_presets() {
        assert_eq!(cadence_label(&daily(17, 0)), "Daily at 5:00 PM");
        assert_eq!(cadence_label(&daily(9, 0)), "Daily at 9:00 AM");
        assert_eq!(
            cadence_label(&weekly(9, 0, "Mon-Fri")),
            "Weekdays at 9:00 AM"
        );
        assert_eq!(
            cadence_label(&weekly(10, 30, "Mon,Wed,Fri")),
            "Mon, Wed, Fri at 10:30 AM"
        );
        assert_eq!(
            cadence_label(&monthly(17, 0, 17)),
            "Monthly on day 17 at 5:00 PM"
        );
        assert_eq!(cadence_label(&hourly(0)), "Hourly");
        // Every-minute is not hourly: a wildcard minute must not be mislabeled.
        assert_ne!(cadence_label("0 * * * * *"), "Hourly");
    }
}
