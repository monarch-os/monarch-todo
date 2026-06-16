//! Command-line interface — the scriptable front-end over the shared store.
//!
//! Mirrors the former `monarch todo` bash group one-for-one, so existing habits
//! and scripts keep working, now from a single Rust implementation of the store.

use crate::store::{Priority, Status, Store, Task};
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "monarch-todo", version, about = "Fast todo manager (TUI + CLI)")]
#[command(long_about = "Run with no subcommand to open the TUI; use a subcommand for scripting.\n\
    Store: $MONARCH_TODO_FILE || ${XDG_STATE_HOME:-$HOME/.local/state}/monarch/todo.json")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
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
    Done {
        id: String,
    },
    /// Set a task's status
    Status {
        id: String,
        status: String,
    },
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
    Rm {
        id: String,
    },
    /// Remove all finished tasks (done and cancelled)
    Clear,
    /// Open the raw store in $EDITOR
    Edit,
    /// Print Noctalia bar-widget JSON (open-task count); empty object hides it
    Bar,
}

pub fn run(command: Command) -> Result<()> {
    let mut store = Store::load()?;
    match command {
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
            let category = category.as_deref().and_then(crate::store::normalize_category);
            let task = store.add(text.trim().to_string(), priority, due, category);
            let id = task.id.clone();
            store.save()?;
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
                group_rank(a.category_key())
                    .cmp(&group_rank(b.category_key()))
                    .then_with(|| a.category_key().cmp(b.category_key()))
                    .then_with(|| crate::store::cmp_tasks(a, b))
            });

            match format.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(&view)?),
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
            store.tasks[i].status = Status::Done;
            store.save()?;
            println!("Completed [{}]", store.tasks[i].id);
        }
        Command::Status { id, status } => {
            let s = parse_status(&status)?;
            let i = store.find(&id).map_err(anyhow::Error::msg)?;
            store.tasks[i].status = s;
            store.save()?;
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
            if let Some(c) = category {
                store.tasks[i].category = crate::store::normalize_category(&c);
            }
            store.save()?;
            println!("Updated [{}]", store.tasks[i].id);
        }
        Command::Rm { id } => {
            let i = store.find(&id).map_err(anyhow::Error::msg)?;
            let task = store.tasks.remove(i);
            store.save()?;
            println!("Removed [{}] {}", task.id, task.text);
        }
        Command::Clear => {
            let before = store.tasks.len();
            store.tasks.retain(|t| t.status.is_open());
            let removed = before - store.tasks.len();
            store.save()?;
            println!("Cleared {} finished task(s).", removed);
        }
        Command::Edit => {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".into());
            // Make sure the file exists so the editor opens something.
            if !store.path.exists() {
                store.save()?;
            }
            let status = std::process::Command::new(editor).arg(&store.path).status()?;
            if !status.success() {
                bail!("editor exited with an error");
            }
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
    Status::parse(s).ok_or_else(|| anyhow::anyhow!("Status must be todo, doing, done, or cancelled."))
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
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
    out
}
