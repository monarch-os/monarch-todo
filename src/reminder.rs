//! Reminder scheduling, reconciled against the store.
//!
//! Each task with a future `reminder` field gets a systemd `--user` timer named
//! `monarch-todo-reminder-<id>` that fires `monarch-todo reminder-fire <id>` at
//! the absolute time. The timers are transient (lost on reboot), so the store's
//! `reminder` field is the source of truth and `reconcile()` re-arms missing
//! timers — run on TUI startup and at login (niri `spawn-at-startup`). This
//! keeps what the UI shows (🔔) and what will actually fire in sync.

use crate::store::{Priority, Store, Task};
use anyhow::Result;
use chrono::{DateTime, Local};
use std::collections::HashSet;
use std::process::Command;

fn unit_name(id: &str) -> String {
    format!("monarch-todo-reminder-{}", id)
}

/// Notification urgency mapped from the task's priority.
pub fn urgency_for(priority: Option<Priority>) -> &'static str {
    match priority {
        Some(Priority::A) => "critical",
        Some(Priority::B) => "normal",
        Some(Priority::C) => "low",
        None => "normal",
    }
}

fn parse_dt(s: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Local))
}

/// Active `monarch-todo-reminder-*` timer unit base names (no `.timer`).
fn live_timers() -> Vec<String> {
    let output = Command::new("systemctl")
        .args([
            "--user",
            "list-units",
            "--no-legend",
            "--plain",
            "monarch-todo-reminder-*.timer",
        ])
        .output();
    let mut units = Vec::new();
    if let Ok(out) = output {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(unit) = line.split_whitespace().next() {
                if let Some(base) = unit.strip_suffix(".timer") {
                    units.push(base.to_string());
                }
            }
        }
    }
    units
}

fn stop_unit(base: &str) {
    // Best-effort cleanup: the unit may not exist (e.g. arming a reminder for
    // the first time). Use `.output()` so systemd's "Unit not loaded" noise is
    // captured instead of leaking to the terminal/TUI.
    for kind in ["timer", "service"] {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &format!("{}.{}", base, kind)])
            .output();
    }
    // Clear any failed state so the unit name can be reused.
    let _ = Command::new("systemctl")
        .args(["--user", "reset-failed", &format!("{}.timer", base)])
        .output();
}

fn create_timer(task: &Task, when: DateTime<Local>) {
    let unit = unit_name(&task.id);
    let calendar = when.format("%Y-%m-%d %H:%M:%S").to_string();
    let _ = Command::new("systemd-run")
        .args([
            "--user",
            "--quiet",
            "--collect",
            &format!("--on-calendar={}", calendar),
            &format!("--unit={}", unit),
            "monarch-todo",
            "reminder-fire",
            &task.id,
        ])
        .status();
}

fn notify(text: &str, urgency: &str) {
    let sent = Command::new("monarch-notification-send")
        .args(["󰔛", "Reminder", text, "-u", urgency])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !sent {
        let _ = Command::new("notify-send")
            .args(["-u", urgency, "󰔛 Reminder", text])
            .status();
    }
}

/// (Re)arm the timer for a single task to match its `reminder` field. Called
/// when a reminder is set/changed from the TUI.
pub fn ensure(task: &Task) {
    stop_unit(&unit_name(&task.id));
    if let Some(when) = task.reminder.as_deref().and_then(parse_dt) {
        if when > Local::now() {
            create_timer(task, when);
        }
    }
}

/// Fire one reminder: notify, then clear the task's `reminder` field. Invoked by
/// the systemd timer (`monarch-todo reminder-fire <id>`).
pub fn fire(id: &str) -> Result<()> {
    let mut store = Store::load()?;
    if let Some(task) = store.tasks.iter().find(|t| t.id == id) {
        if task.reminder.is_some() {
            notify(&task.text, urgency_for(task.priority));
        }
    }
    let mut changed = false;
    for task in store.tasks.iter_mut() {
        if task.id == id && task.reminder.is_some() {
            task.reminder = None;
            changed = true;
        }
    }
    if changed {
        store.save()?;
    }
    Ok(())
}

/// Make systemd timers match the store: create missing timers for future
/// reminders, fire + clear past ones (missed during downtime), and drop orphan
/// timers. Idempotent — safe to run at every TUI startup and at login.
pub fn reconcile() -> Result<()> {
    let mut store = Store::load()?;
    let now = Local::now();
    let live = live_timers();

    // Immutable pass: classify reminders.
    let mut future: Vec<(Task, DateTime<Local>)> = Vec::new();
    let mut missed: Vec<Task> = Vec::new();
    for task in &store.tasks {
        match task.reminder.as_deref().and_then(parse_dt) {
            Some(when) if when > now => future.push((task.clone(), when)),
            Some(_) => missed.push(task.clone()),
            None => {}
        }
    }
    let wanted: HashSet<String> = future.iter().map(|(t, _)| unit_name(&t.id)).collect();

    // Side effects (no store borrow held).
    for (task, when) in &future {
        if !live.contains(&unit_name(&task.id)) {
            create_timer(task, *when);
        }
    }
    for task in &missed {
        notify(&task.text, urgency_for(task.priority));
        stop_unit(&unit_name(&task.id));
    }
    for base in &live {
        if !wanted.contains(base) {
            stop_unit(base);
        }
    }

    // Mutable pass: clear past reminder fields.
    let mut changed = false;
    for task in store.tasks.iter_mut() {
        if task
            .reminder
            .as_deref()
            .and_then(parse_dt)
            .is_some_and(|when| when <= now)
        {
            task.reminder = None;
            changed = true;
        }
    }
    if changed {
        store.save()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_mapping() {
        assert_eq!(urgency_for(Some(Priority::A)), "critical");
        assert_eq!(urgency_for(Some(Priority::B)), "normal");
        assert_eq!(urgency_for(Some(Priority::C)), "low");
        assert_eq!(urgency_for(None), "normal");
    }

    #[test]
    fn unit_name_and_parse() {
        assert_eq!(unit_name("ab12cd"), "monarch-todo-reminder-ab12cd");
        assert!(parse_dt("2026-06-17T08:59:43+02:00").is_some());
        assert!(parse_dt("nope").is_none());
    }
}
