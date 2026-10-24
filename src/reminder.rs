//! systemd owns delivery while the panel is closed. The locked store owns state.
use crate::store::{Priority, Store, Task};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use std::collections::HashSet;
use std::process::Command;

pub fn urgency_for(priority: Option<Priority>) -> &'static str {
    match priority {
        Some(Priority::Critical) => "critical",
        Some(Priority::C) => "low",
        _ => "normal",
    }
}

fn notify(task: &Task) -> Result<()> {
    let text = &task.text;
    let urgency = urgency_for(task.priority);
    // Monarch's notification helper accepts options after its headline too.
    // A task title must never be interpreted as one of those options.
    let body = if text.starts_with('-') {
        format!(" {text}")
    } else {
        text.to_owned()
    };
    let action = crate::desktop::text("todo.snoozeNotification");
    let sent = Command::new("monarch-notification-send")
        .args([
            "-u", urgency, "-g", "󰔛", "Reminder", &body, "--action", &action,
        ])
        .arg(std::env::current_exe()?)
        .args(["snooze", &task.id, "+5m"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !sent
        && !Command::new("notify-send")
            .args(["-u", urgency, "--", "Reminder", text])
            .status()?
            .success()
    {
        bail!("Could not deliver reminder notification");
    }
    Ok(())
}

fn active_timers() -> Result<Vec<String>> {
    let out = Command::new("systemctl")
        .args([
            "--user",
            "list-units",
            "--state=active",
            "--no-legend",
            "--plain",
            "monarch-todo-reminder-*.timer",
        ])
        .output()?;
    if !out.status.success() {
        bail!("Cannot list user reminder timers");
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|unit| unit.strip_suffix(".timer").map(str::to_owned))
        .collect())
}

/// Date-qualified units let edits replace a timer without re-arming unchanged ones.
/// Never stop a service: it might itself be waiting for this transaction's lock.
pub fn sync(store: &Store) -> Result<()> {
    let now = Local::now();
    let mut wanted = HashSet::new();
    let live: HashSet<String> = active_timers()?.into_iter().collect();
    for task in &store.tasks {
        if !task.status.is_open() || task.notified {
            continue;
        }
        let Some(value) = &task.reminder else {
            continue;
        };
        let date = DateTime::parse_from_rfc3339(value).context("Invalid stored reminder")?;
        if date <= now {
            continue;
        }
        // OnCalendar below has second precision; never schedule before a
        // fractional stored timestamp, or fire() would correctly reject it.
        let scheduled = if date.timestamp_subsec_nanos() > 0 {
            date + chrono::Duration::seconds(1)
        } else {
            date
        };
        let unit = format!(
            "monarch-todo-reminder-{}-{}-{}",
            task.id,
            date.timestamp(),
            date.timestamp_subsec_nanos()
        );
        wanted.insert(unit.clone());
        if live.contains(&unit) {
            continue;
        }
        let _ = Command::new("systemctl")
            .args(["--user", "reset-failed", &format!("{unit}.service")])
            .output();
        let mut cmd = Command::new("systemd-run");
        cmd.args([
            "--user",
            "--quiet",
            "--collect",
            "--timer-property=AccuracySec=1s",
            // The notification helper forks an action listener. Keep its cgroup
            // alive until the notification is acted on/dismissed, not just until
            // reminder-fire exits; default ExitType=main kills that listener.
            "--property=ExitType=cgroup",
            &format!("--unit={unit}"),
            &format!(
                "--on-calendar={}",
                scheduled
                    .with_timezone(&chrono::Utc)
                    .format("%Y-%m-%d %H:%M:%S UTC")
            ),
        ]);
        // Store overrides must survive invocation from systemd (also useful for tests).
        cmd.arg(format!(
            "--setenv=MONARCH_TODO_FILE={}",
            std::path::absolute(&store.path)?.display()
        ));
        let status = cmd
            .arg(std::env::current_exe()?)
            .args(["reminder-fire", &task.id, "--expected", value])
            .status()?;
        if !status.success() {
            bail!("Task saved, but its reminder timer could not be scheduled; run reload-reminders to retry");
        }
    }
    for unit in live {
        if !wanted.contains(&unit) {
            let status = Command::new("systemctl")
                .args(["--user", "stop", &format!("{unit}.timer")])
                .status()?;
            if !status.success() {
                bail!("Could not stop obsolete reminder timer");
            }
        }
    }
    Ok(())
}

pub fn fire(id: &str, expected: Option<&str>) -> Result<()> {
    let mut store = Store::load()?;
    let Some(task) = store.tasks.iter_mut().find(|t| t.id == id) else {
        return Ok(());
    };
    if !task.status.is_open() || task.notified {
        return Ok(());
    }
    let Some(value) = &task.reminder else {
        return Ok(());
    };
    let date = DateTime::parse_from_rfc3339(value)?;
    if expected.is_some_and(|e| {
        DateTime::parse_from_rfc3339(e)
            .map(|e| e != date)
            .unwrap_or(true)
    }) {
        return Ok(());
    }
    if date > Local::now() {
        return Ok(());
    }
    notify(task)?;
    // Keep the cadence anchor until the recurring occurrence is completed.
    task.notified = true;
    if task.recurrence.is_none() {
        task.reminder = None;
    }
    store.save()
}

pub fn reconcile() -> Result<()> {
    let mut store = Store::load()?;
    let now = Local::now();
    for index in 0..store.tasks.len() {
        let task = &mut store.tasks[index];
        if !task.status.is_open() || task.notified {
            continue;
        }
        let Some(value) = &task.reminder else {
            continue;
        };
        if DateTime::parse_from_rfc3339(value)? <= now {
            notify(task)?;
            task.notified = true;
            if task.recurrence.is_none() {
                task.reminder = None;
            }
            // Persist each successful delivery before attempting the next one.
            store.save()?;
        }
    }
    sync(&store)
}
