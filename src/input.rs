//! Walker-style input. Only the prefix before `>` is interpreted as a date.
//! Natural English dates use interim; French words are normalized explicitly.
use crate::store::Priority;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Datelike, Days, Local, Months, NaiveTime, TimeZone, Timelike};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    Minutes,
    Hours,
    Days,
    Weeks,
    Months,
    Years,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Recurrence {
    pub every: u32,
    pub unit: Unit,
    #[serde(default)]
    pub anchor_day: Option<u32>,
}

impl Recurrence {
    pub fn parse(value: &str) -> Result<Self> {
        let value = match value {
            "daily" | "quotidien" => "1d",
            "weekly" | "hebdomadaire" => "1w",
            "monthly" | "mensuel" => "1mo",
            "yearly" | "annuel" => "1y",
            other => other.strip_prefix("every:").unwrap_or(other),
        };
        let end = value
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(value.len());
        let every: u32 = value[..end]
            .parse()
            .context("Expected @daily, @weekly, @monthly or @every:2d")?;
        if every == 0 || every > 10000 {
            bail!("Recurrence interval must be between 1 and 10000");
        }
        let unit = match &value[end..] {
            "m" => Unit::Minutes,
            "h" => Unit::Hours,
            "d" | "j" => Unit::Days,
            "w" | "s" => Unit::Weeks,
            "mo" => Unit::Months,
            "y" | "a" => Unit::Years,
            _ => bail!("Unknown recurrence unit (m, h, d, w, mo, y)"),
        };
        Ok(Self {
            every,
            unit,
            anchor_day: None,
        })
    }

    /// Calendar-based cadence, anchored to the original due date, not completion.
    /// Missed occurrences are skipped, not expanded into hundreds of tasks.
    pub fn next_after(
        &self,
        base: DateTime<Local>,
        now: DateTime<Local>,
    ) -> Result<DateTime<Local>> {
        if self.every == 0 {
            bail!("Invalid zero recurrence interval");
        }
        for step in 1u32..=1_000_000 {
            let n = self
                .every
                .checked_mul(step)
                .context("Recurrence overflow")?;
            let next = match self.unit {
                Unit::Minutes | Unit::Hours => {
                    let seconds = i64::from(n) * if self.unit == Unit::Hours { 3600 } else { 60 };
                    base.checked_add_signed(chrono::Duration::seconds(seconds))
                }
                _ => {
                    let mut date = match self.unit {
                        Unit::Days => base.date_naive().checked_add_days(Days::new(u64::from(n))),
                        Unit::Weeks => base
                            .date_naive()
                            .checked_add_days(Days::new(u64::from(n) * 7)),
                        Unit::Months => base.date_naive().checked_add_months(Months::new(n)),
                        Unit::Years => n
                            .checked_mul(12)
                            .and_then(|n| base.date_naive().checked_add_months(Months::new(n))),
                        _ => unreachable!(),
                    }
                    .context("Recurrence date overflow")?;
                    if matches!(self.unit, Unit::Months | Unit::Years) {
                        let last_day = (28..=31)
                            .rev()
                            .find(|day| date.with_day(*day).is_some())
                            .unwrap();
                        let day = self.anchor_day.unwrap_or(base.day()).min(last_day);
                        date = date
                            .with_day(day)
                            .context("Invalid recurrence anchor day")?;
                    }
                    // Reject nonexistent local times rather than silently changing the hour.
                    Local
                        .from_local_datetime(&date.and_time(base.time()))
                        .earliest()
                }
            };
            if let Some(next) = next {
                if next > now {
                    return Ok(next);
                }
            }
        }
        bail!("Too many missed recurrence intervals")
    }
}

#[derive(Debug, Serialize)]
pub struct Parsed {
    pub text: String,
    pub priority: Option<Priority>,
    pub category: Option<String>,
    pub reminder: Option<String>,
    pub recurrence: Option<Recurrence>,
}

fn input_words(body: &str) -> Result<Vec<&str>> {
    let mut words = Vec::new();
    let mut cursor = 0;
    while cursor < body.len() {
        let character = body[cursor..].chars().next().unwrap();
        if character.is_whitespace() {
            cursor += character.len_utf8();
            continue;
        }
        let start = cursor;
        if body[cursor..].starts_with("#(") {
            cursor += 2;
            let mut depth = 1;
            while cursor < body.len() && depth > 0 {
                let character = body[cursor..].chars().next().unwrap();
                cursor += character.len_utf8();
                match character {
                    '\\' if cursor < body.len() => {
                        cursor += body[cursor..].chars().next().unwrap().len_utf8()
                    }
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
            }
            if depth != 0 {
                bail!("Unclosed category; expected #(Category name)");
            }
            if cursor < body.len() && !body[cursor..].chars().next().unwrap().is_whitespace() {
                bail!("Separate category suffixes with spaces");
            }
        } else {
            while cursor < body.len() {
                let character = body[cursor..].chars().next().unwrap();
                if character.is_whitespace() {
                    break;
                }
                cursor += character.len_utf8();
            }
        }
        words.push(&body[start..cursor]);
    }
    Ok(words)
}

fn category_text(value: &str) -> String {
    let mut text = String::new();
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\\'
            && chars
                .peek()
                .is_some_and(|next| matches!(next, '(' | ')' | '\\'))
        {
            text.push(chars.next().unwrap());
        } else {
            text.push(character);
        }
    }
    text
}

pub fn parse(query: &str, now: DateTime<Local>) -> Result<Parsed> {
    let (date, body) = match query.split_once('>') {
        Some((date, body)) => (
            if date.trim().is_empty() {
                None
            } else {
                Some(date.trim())
            },
            body.trim(),
        ),
        None => (None, query.trim()),
    };
    let mut words = input_words(body)?;
    let mut priority = None;
    let mut recurrence = None;
    let mut category = None;
    // Suffix tokens only; a backslash keeps a token literally in the title.
    while let Some(last) = words.last().copied() {
        if let Some(value) = last.strip_prefix('#') {
            if category.is_some() {
                bail!("Specify only one category per task");
            }
            let value = if value.starts_with('(') && value.ends_with(')') {
                category_text(&value[1..value.len() - 1])
            } else {
                value.to_owned()
            };
            category = Some(
                crate::store::normalize_category(&value).context("Category name is required")?,
            );
        } else if let Some(value) = last.strip_prefix('!') {
            if priority.is_some() {
                bail!("Specify only one priority");
            }
            priority = Some(if value.is_empty() {
                Priority::Critical
            } else {
                Priority::parse(value).context("Unknown priority: low, normal, high, critical")?
            });
        } else if let Some(value) = last.strip_prefix('@') {
            if recurrence.is_some() {
                bail!("Specify only one recurrence");
            }
            recurrence = Some(Recurrence::parse(&value.to_lowercase())?);
        } else if last.ends_with('!') && !last.starts_with('\\') && priority.is_none() {
            priority = Some(Priority::Critical);
            *words.last_mut().unwrap() = &last[..last.len() - 1];
            break;
        } else {
            break;
        }
        words.pop();
    }
    let text = words
        .iter()
        .map(|word| word.strip_prefix('\\').unwrap_or(word))
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        bail!("Task text is required");
    }
    let reminder = date.map(|date| parse_date(date, now)).transpose()?;
    if let (Some(rule), Some(date)) = (&mut recurrence, reminder) {
        rule.anchor_day = Some(date.day());
    }
    if recurrence.is_some() && reminder.is_none() {
        bail!("A recurring task needs a first date, e.g. tomorrow at 9am > Task @daily");
    }
    Ok(Parsed {
        text,
        priority,
        category,
        reminder: reminder.map(|d| d.to_rfc3339()),
        recurrence,
    })
}

pub fn parse_date(value: &str, now: DateTime<Local>) -> Result<DateTime<Local>> {
    // RFC3339 is used by the edit round-trip, preserving offsets and seconds.
    if let Ok(date) = DateTime::parse_from_rfc3339(value) {
        return Ok(date.with_timezone(&Local));
    }
    let words = value
        .to_lowercase()
        .replace('’', "'")
        .replace("aujourd'hui", "today")
        .replace("après-demain", "2 days")
        .replace("tomorow", "tomorrow");
    let mut normalized = Vec::new();
    for word in words.split_whitespace() {
        normalized.push(
            match word {
                "in" | "dans" | "at" | "à" => continue,
                "demain" => "tomorrow",
                "hier" => "yesterday",
                "maintenant" => "now",
                "lundi" => "monday",
                "mardi" => "tuesday",
                "mercredi" => "wednesday",
                "jeudi" => "thursday",
                "vendredi" => "friday",
                "samedi" => "saturday",
                "dimanche" => "sunday",
                "prochain" | "prochaine" => "next",
                "minute" | "minutes" => "minutes",
                "heure" | "heures" => "hours",
                "jour" | "jours" => "days",
                "semaine" | "semaines" => "weeks",
                "mois" => "months",
                "an" | "ans" | "année" | "années" => "years",
                "janvier" => "january",
                "février" => "february",
                "mars" => "march",
                "avril" => "april",
                "mai" => "may",
                "juin" => "june",
                "juillet" => "july",
                "août" => "august",
                "septembre" => "september",
                "octobre" => "october",
                "novembre" => "november",
                "décembre" => "december",
                other => other,
            }
            .to_owned(),
        );
    }
    // French places 'next' after the weekday/month; interim expects it first.
    if let Some(index) = normalized.iter().position(|word| word == "next") {
        normalized.remove(index);
        normalized.insert(0, "next".into());
    }
    let mut expr = normalized.join(" ");
    if expr.is_empty() {
        bail!("A date is required before >");
    }
    expr = expr.trim_start_matches('+').to_owned();
    // French clock notation: 9h or 9h30 (bare +1h remains a duration).
    if normalized.len() > 1 {
        if let Some(last) = normalized.last() {
            if let Some((hour, minute)) = last.split_once('h') {
                if hour.parse::<u32>().is_ok()
                    && (minute.is_empty() || minute.parse::<u32>().is_ok())
                {
                    let clock = format!(
                        "{}:{:0>2}",
                        hour,
                        if minute.is_empty() { "00" } else { minute }
                    );
                    expr = format!("{} {}", normalized[..normalized.len() - 1].join(" "), clock);
                }
            }
        }
    }
    // interim normalizes invalid clock hours (25:00 becomes the next day).
    // Reject that surprising interpretation before offering the preview.
    for word in expr.split_whitespace() {
        let ampm = word.ends_with("am") || word.ends_with("pm");
        let clock = if ampm { &word[..word.len() - 2] } else { word };
        if clock.contains(':') || ampm {
            let parts: Vec<&str> = clock.split(':').collect();
            let hour: u32 = parts[0].parse().context("Invalid clock hour")?;
            if hour > if ampm { 12 } else { 23 } || (ampm && hour == 0) || parts.len() > 3 {
                bail!("Invalid clock hour");
            }
            for part in &parts[1..] {
                if part.parse::<u32>().context("Invalid clock minute/second")? > 59 {
                    bail!("Invalid clock minute/second");
                }
            }
        }
    }
    let date: DateTime<Local> = interim::parse_date_string(&expr, now, interim::Dialect::Uk)
        .map_err(|e| anyhow::anyhow!("Unrecognized date '{value}': {e}"))?;
    let has_clock = expr.contains(':') || expr.contains("am") || expr.contains("pm");
    let relative =
        expr.starts_with(|c: char| c.is_ascii_digit()) && interim::parse_duration(&expr).is_ok();
    let date = if !has_clock && !relative && expr != "now" {
        Local
            .from_local_datetime(
                &date
                    .date_naive()
                    .and_time(NaiveTime::from_hms_opt(23, 59, 59).unwrap()),
            )
            .single()
            .context("Ambiguous local date")?
    } else {
        date
    };
    if date <= now {
        bail!("The scheduled time is in the past");
    }
    date.with_nanosecond(0).context("Invalid date precision")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 23, 12, 0, 0).unwrap()
    }
    #[test]
    fn natural_dates() {
        for text in ["+5m", "in 5m", "dans 5 minutes"] {
            assert_eq!(
                parse_date(text, now()).unwrap(),
                now() + chrono::Duration::minutes(5)
            );
        }
        assert_eq!(parse_date("tomorrow at 09:00", now()).unwrap().hour(), 9);
        assert_eq!(parse_date("demain à 9h30", now()).unwrap().minute(), 30);
        assert_eq!(parse_date("tomorow", now()).unwrap().day(), 24);
        assert_eq!(parse_date("today", now()).unwrap().hour(), 23);
        assert_eq!(parse_date("aujourd’hui", now()).unwrap().hour(), 23);
        assert_eq!(
            parse_date("vendredi prochain à 9h", now()).unwrap().hour(),
            9
        );
        assert!(parse_date("tomorrow at 25:00", now()).is_err());
        assert!(parse_date("tomorrow at 9:65", now()).is_err());
        assert!(parse_date("nonsense", now()).is_err());
        assert!(parse_date("yesterday", now()).is_err());
    }
    #[test]
    fn suffixes_and_literals() {
        let task = parse("tomorrow at 9am > Appeler !critical @daily #Maison", now()).unwrap();
        assert_eq!(task.text, "Appeler");
        assert_eq!(task.priority, Some(Priority::Critical));
        assert_eq!(task.category.as_deref(), Some("Maison"));
        assert!(task.recurrence.is_some());
        assert_eq!(parse("Call tomorrow", now()).unwrap().text, "Call tomorrow");
        assert_eq!(
            parse(r"Literal \!high", now()).unwrap().text,
            "Literal !high"
        );
        assert!(parse("Task @daily", now()).is_err());
        assert!(parse("bad-date > Task", now()).is_err());
        assert!(parse("Task !banana", now()).is_err());
    }
    #[test]
    fn recurrence_skips_missed_occurrences() {
        let rule = Recurrence::parse("daily").unwrap();
        let base = now() - chrono::Duration::days(10);
        assert_eq!(
            rule.next_after(base, now()).unwrap().date_naive(),
            now().date_naive() + Days::new(1)
        );
        assert!(Recurrence::parse("every:0d").is_err());
        assert!(Recurrence::parse("every:2bananas").is_err());
    }
    #[test]
    fn month_end_is_clamped_without_drifting_in_one_advance() {
        let base = Local.with_ymd_and_hms(2026, 1, 31, 9, 0, 0).unwrap();
        let after = Local.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap();
        assert_eq!(
            Recurrence::parse("monthly")
                .unwrap()
                .next_after(base, after)
                .unwrap()
                .day(),
            31
        );
    }
}
