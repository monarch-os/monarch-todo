//! Command-line interface — the scriptable front-end over the shared store.
//!
//! Mirrors the former `monarch todo` bash group one-for-one, so existing habits
//! and scripts keep working, now from a single Rust implementation of the store.

use crate::store::{Priority, Status, Store, Task};
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "monarch-todo",
    version,
    about = "Task and reminder backend for Monarch"
)]
#[command(
    long_about = "Use the Noctalia panel for interactive tasks, or these commands for scripting.\n\
    Store: $MONARCH_TODO_FILE || ${XDG_STATE_HOME:-$HOME/.local/state}/monarch/todo.json"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Open the Noctalia panel (register its packaged plugin if needed)
    Panel,
    /// Register the packaged plugin and enable reminders for this user
    SetupDesktop,
    /// Disable this user's plugin/reminders, preserving data and custom files
    RemoveDesktop,
    /// Print the panel's translated strings as JSON
    Strings,
    /// Parse Walker-style input and print JSON without saving anything
    Parse { query: String },
    /// Create a task from Walker-style input and print its JSON
    Quick { query: String },
    /// Replace task fields using the same input syntax (keeps its ID and history)
    Update { id: String, query: String },
    /// Set or remove a reminder; use '-' to remove it
    Reminder { id: String, when: String },
    /// Delay a notification without shifting the recurring task's cadence
    Snooze { id: String, when: String },
    /// Undo the last user mutation, refusing conflicting later changes
    Undo,
    /// Add a task
    Add {
        /// Task text (joined)
        text: Vec<String>,
        #[arg(short = 'p', long, value_name = "high|medium|low")]
        priority: Option<String>,
        #[arg(short = 'd', long, value_name = "YYYY-MM-DD")]
        due: Option<String>,
        #[arg(short = 'c', long)]
        category: Option<String>,
    },
    /// List tasks, grouped by category, sorted by status/priority/due
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        done: bool,
        #[arg(long, value_name = "todo|doing|done|cancelled")]
        status: Option<String>,
        #[arg(long)]
        category: Option<String>,
        #[arg(long, default_value = "plain", value_name = "plain|json")]
        format: String,
    },
    /// Mark a task as done
    Done { id: String },
    /// Set a task's status
    Status { id: String, status: String },
    /// Update a task's text, priority, due date, or tags
    Set {
        id: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long, value_name = "high|medium|low|-")]
        priority: Option<String>,
        #[arg(long, value_name = "YYYY-MM-DD|-")]
        due: Option<String>,
        #[arg(long, value_name = "name|-")]
        category: Option<String>,
    },
    /// Remove a task
    Rm { id: String },
    /// Remove all finished tasks (done and cancelled)
    Clear,
    /// Open the raw store in $EDITOR
    Edit,
    /// Print Noctalia bar-widget JSON (open-task count); empty object hides it
    Bar,
    /// Re-arm reminder timers and deliver missed reminders (run at login)
    ReloadReminders,
    /// Fire a task's reminder, then clear it (invoked by the systemd timer)
    #[command(hide = true)]
    ReminderFire {
        id: String,
        #[arg(long)]
        expected: Option<String>,
    },
}

pub fn run(command: Command) -> Result<()> {
    // These operations own their store transaction, or need none. Never acquire
    // the store lock twice in one process.
    let command = match command {
        Command::Panel => return crate::desktop::run("open"),
        Command::SetupDesktop => return crate::desktop::run("setup"),
        Command::RemoveDesktop => return crate::desktop::run("remove"),
        Command::Strings => {
            println!("{}", crate::desktop::strings());
            return Ok(());
        }
        Command::Parse { query } => {
            println!(
                "{}",
                serde_json::to_string(&crate::input::parse(&query, chrono::Local::now())?)?
            );
            return Ok(());
        }
        Command::ReloadReminders => return crate::reminder::reconcile(),
        Command::ReminderFire { id, expected } => {
            return crate::reminder::fire(&id, expected.as_deref())
        }
        other => other,
    };
    let mut store = Store::load()?;
    // Listing/bar polling never needs an undo snapshot or timer reconciliation.
    let mutated = !matches!(&command, Command::List { .. } | Command::Bar);
    let before = if mutated && !matches!(&command, Command::Undo | Command::Edit) {
        store.tasks.clone()
    } else {
        Vec::new()
    };
    match command {
        Command::Undo => {
            store.undo()?;
            println!("Undone");
            drop(store);
            return crate::reminder::reconcile();
        }
        Command::Snooze { id, when } => {
            let i = store.find(&id).map_err(anyhow::Error::msg)?;
            store.snooze(i, &when)?;
            store.save_user_change(&before)?;
            println!("Snoozed [{}]", store.tasks[i].id);
        }
        Command::Quick { query } => {
            let parsed = crate::input::parse(&query, chrono::Local::now())?;
            if let Some(date) = &parsed.reminder {
                if chrono::DateTime::parse_from_rfc3339(date)? <= chrono::Local::now() {
                    bail!("The scheduled time is in the past");
                }
            }
            let due = parsed.reminder.as_ref().map(|d| d[..10].to_owned());
            let task = store.add(parsed.text, parsed.priority, due, parsed.category);
            task.reminder = parsed.reminder;
            task.recurrence = parsed.recurrence;
            let json = serde_json::to_string(task)?;
            store.save_user_change(&before)?;
            println!("{json}");
        }
        Command::Update { id, query } => {
            let parsed = crate::input::parse(&query, chrono::Local::now())?;
            let i = store.find(&id).map_err(anyhow::Error::msg)?;
            if !store.tasks[i].status.is_open() {
                bail!("Reopen the task before editing it");
            }
            let task = &mut store.tasks[i];
            let same_schedule = match (&task.reminder, &parsed.reminder) {
                (None, None) => true,
                (Some(old), Some(new)) => chrono::DateTime::parse_from_rfc3339(old)
                    .ok()
                    .zip(chrono::DateTime::parse_from_rfc3339(new).ok())
                    .is_some_and(|(old, new)| old == new),
                _ => false,
            };
            if !same_schedule {
                if let Some(date) = &parsed.reminder {
                    if chrono::DateTime::parse_from_rfc3339(date)? <= chrono::Local::now() {
                        bail!("The scheduled time is in the past");
                    }
                }
            }
            let old_anchor = task.recurrence.as_ref().and_then(|r| r.anchor_day);
            let same_rule = match (&task.recurrence, &parsed.recurrence) {
                (Some(old), Some(new)) => old.every == new.every && old.unit == new.unit,
                (None, None) => true,
                _ => false,
            };
            if !same_schedule || !same_rule {
                task.recurrence_anchor = None;
            }
            task.text = parsed.text;
            task.priority = parsed.priority;
            task.category = parsed.category;
            if let Some(date) = &parsed.reminder {
                task.due = Some(date[..10].to_owned());
            } else if task.reminder.is_some() {
                task.due = None;
            }
            task.reminder = parsed.reminder;
            task.recurrence = parsed.recurrence;
            if same_schedule {
                if same_rule {
                    if let Some(rule) = &mut task.recurrence {
                        rule.anchor_day = old_anchor.or(rule.anchor_day);
                    }
                }
            } else {
                task.notified = false;
            }
            store.save_user_change(&before)?;
        }
        Command::Reminder { id, when } => {
            let i = store.find(&id).map_err(anyhow::Error::msg)?;
            let task = &mut store.tasks[i];
            if !task.status.is_open() {
                bail!("Reopen the task before scheduling a reminder");
            }
            if when == "-" {
                task.reminder = None;
                task.recurrence = None;
            } else {
                let date = crate::input::parse_date(&when, chrono::Local::now())?;
                if date <= chrono::Local::now() {
                    bail!("The scheduled time is in the past");
                }
                task.reminder = Some(date.to_rfc3339());
                task.due = Some(date.format("%Y-%m-%d").to_string());
                if let Some(rule) = &mut task.recurrence {
                    use chrono::Datelike;
                    rule.anchor_day = Some(date.day());
                }
            }
            task.notified = false;
            task.recurrence_anchor = None;
            store.save_user_change(&before)?;
        }
        Command::Add {
            text,
            priority,
            due,
            category,
        } => {
            let text = text.join(" ");
            if text.trim().is_empty() {
                bail!("Task text is required.");
            }
            let priority = parse_priority_opt(priority.as_deref())?;
            let due = parse_due_opt(due.as_deref())?;
            let task = store.add(text.trim().to_string(), priority, due, category);
            let id = task.id.clone();
            store.save_user_change(&before)?;
            println!("Added [{}]", id);
        }
        Command::List {
            all,
            done,
            status,
            category,
            format,
        } => {
            let scope_status = status.as_deref().map(parse_status).transpose()?;
            let mut view: Vec<&Task> = store
                .tasks
                .iter()
                .filter(|t| {
                    if let Some(s) = scope_status {
                        t.status == s
                    } else if all {
                        true
                    } else if done {
                        !t.status.is_open()
                    } else {
                        t.status.is_open()
                    }
                })
                .filter(|t| match &category {
                    Some(c) => t.category_key() == c,
                    None => true,
                })
                .collect();
            view.sort_by(|a, b| {
                if format == "json" {
                    return crate::store::cmp_tasks(a, b);
                }
                group_rank(a.category_key())
                    .cmp(&group_rank(b.category_key()))
                    .then_with(|| a.category_key().cmp(b.category_key()))
                    .then_with(|| crate::store::cmp_tasks(a, b))
            });

            match format.as_str() {
                "json" => {
                    let now = chrono::Local::now().timestamp_millis();
                    let payload = view
                        .iter()
                        .map(|task| {
                            let mut value = serde_json::to_value(task)?;
                            value["elapsed_ms"] = task.elapsed_ms_at(now).into();
                            Ok(value)
                        })
                        .collect::<Result<Vec<serde_json::Value>>>()?;
                    println!("{}", serde_json::to_string_pretty(&payload)?);
                }
                "plain" => {
                    if view.is_empty() {
                        println!("No tasks.");
                    } else {
                        let mut current = None;
                        for t in view {
                            let key = t.category_key();
                            if current != Some(key) {
                                let name = if key.is_empty() { "Uncategorized" } else { key };
                                println!("\n{}", name);
                                current = Some(key);
                            }
                            println!("  {}  {}", t.id, render_plain(t));
                        }
                    }
                }
                other => bail!("Unknown format: {} (expected plain or json)", other),
            }
        }
        Command::Done { id } => {
            let i = store.find(&id).map_err(anyhow::Error::msg)?;
            store.set_status(i, Status::Done)?;
            store.save_user_change(&before)?;
            println!("Completed [{}]", store.tasks[i].id);
        }
        Command::Status { id, status } => {
            let s = parse_status(&status)?;
            let i = store.find(&id).map_err(anyhow::Error::msg)?;
            store.set_status(i, s)?;
            store.save_user_change(&before)?;
            println!("[{}] → {}", store.tasks[i].id, status.to_lowercase());
        }
        Command::Set {
            id,
            text,
            priority,
            due,
            category,
        } => {
            if text.is_none() && priority.is_none() && due.is_none() && category.is_none() {
                bail!("Nothing to update. Pass at least one of --text, --priority, --due, --category.");
            }
            let i = store.find(&id).map_err(anyhow::Error::msg)?;
            if let Some(t) = text {
                if t.trim().is_empty() {
                    bail!("Task text cannot be empty.");
                }
                store.tasks[i].text = t;
            }
            if let Some(p) = priority {
                store.tasks[i].priority = parse_priority_opt(Some(&p))?;
            }
            if let Some(d) = due {
                store.tasks[i].due = parse_due_opt(Some(&d))?;
            }
            if let Some(category) = category {
                store.tasks[i].category = crate::store::normalize_category(&category);
            }
            store.save_user_change(&before)?;
            println!("Updated [{}]", store.tasks[i].id);
        }
        Command::Rm { id } => {
            let i = store.find(&id).map_err(anyhow::Error::msg)?;
            let task = store.tasks.remove(i);
            store.save_user_change(&before)?;
            println!("Removed [{}] {}", task.id, task.text);
        }
        Command::Clear => {
            let before_count = store.tasks.len();
            store.tasks.retain(|t| t.status.is_open());
            let removed = before_count - store.tasks.len();
            store.save_user_change(&before)?;
            println!("Cleared {} finished task(s).", removed);
        }
        Command::Edit => {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".into());
            // Make sure the file exists so the editor opens something.
            if !store.path.exists() {
                store.save()?;
            }
            let status = std::process::Command::new(editor)
                .arg(&store.path)
                .status()?;
            if !status.success() {
                bail!("editor exited with an error");
            }
            drop(store);
            return crate::reminder::reconcile();
        }
        Command::Bar => {
            let open = store.tasks.iter().filter(|t| t.status.is_open()).count();
            if open == 0 {
                // Empty object → the CustomButton collapses (hideMode expandWithOutput).
                println!("{{}}");
            } else {
                let tooltip = format!("{} open task{}", open, if open == 1 { "" } else { "s" });
                let out = serde_json::json!({ "text": open.to_string(), "tooltip": tooltip });
                println!("{}", out);
            }
        }
        Command::Panel
        | Command::SetupDesktop
        | Command::RemoveDesktop
        | Command::Strings
        | Command::Parse { .. }
        | Command::ReloadReminders
        | Command::ReminderFire { .. } => {
            unreachable!()
        }
    }
    // Synchronize every write, not only the interactive UI. Holding the same
    // lock prevents stale processes from cancelling a newer timer.
    if mutated {
        crate::reminder::sync(&store)?;
    }
    Ok(())
}

/// `-` or empty clears; otherwise A/B/C or high/medium/low.
fn parse_priority_opt(s: Option<&str>) -> Result<Option<Priority>> {
    match s {
        None => Ok(None),
        Some(v) if v.is_empty() || v == "-" => Ok(None),
        Some(v) => match Priority::parse(v) {
            Some(p) => Ok(Some(p)),
            None => bail!("Priority must be A/B/C or high/medium/low (or - to clear)."),
        },
    }
}

/// `-` or empty clears; otherwise YYYY-MM-DD.
fn parse_due_opt(s: Option<&str>) -> Result<Option<String>> {
    match s {
        None => Ok(None),
        Some(v) if v.is_empty() || v == "-" => Ok(None),
        Some(v) => {
            if is_iso_date(v) {
                Ok(Some(v.to_string()))
            } else {
                bail!("Due date must be YYYY-MM-DD (or - to clear).")
            }
        }
    }
}

fn parse_status(s: &str) -> Result<Status> {
    Status::parse(s)
        .ok_or_else(|| anyhow::anyhow!("Status must be todo, doing, done, or cancelled."))
}

fn is_iso_date(s: &str) -> bool {
    s.len() == 10 && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// Uncategorized tasks (key "") sort after named categories.
fn group_rank(key: &str) -> u8 {
    if key.is_empty() {
        1
    } else {
        0
    }
}

/// Plain one-line rendering: marker, coloured priority dot, text, ⏰due.
/// (Category is shown as a group header by the caller.)
fn render_plain(t: &Task) -> String {
    let mut out = String::from(t.status.marker());
    out.push(' ');
    if let Some(p) = t.priority {
        out.push_str(p.dot());
        out.push(' ');
    }
    out.push_str(&t.text);
    if let Some(due) = &t.due {
        out.push_str("  ⏰");
        out.push_str(due);
    }
    if let Some(category) = &t.category {
        out.push_str(&format!("  #{category}"));
    }
    let seconds = t.elapsed_ms_at(chrono::Local::now().timestamp_millis()) / 1_000;
    if seconds > 0 || t.started_at_ms.is_some() {
        out.push_str(&format!(
            "  ⏱ {:02}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        ));
    }
    out
}
