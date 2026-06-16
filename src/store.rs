//! Shared todo store.
//!
//! Reads and writes the very same JSON file the `monarch todo` bash CLI uses,
//! so the Rust TUI and the shell commands are two front-ends over one store.
//! Path resolution mirrors the bash side exactly:
//!   $MONARCH_TODO_FILE  ||  ${XDG_STATE_HOME:-$HOME/.local/state}/monarch/todo.json

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Todo,
    Doing,
    Done,
    Cancelled,
}

impl Status {
    pub fn marker(self) -> &'static str {
        match self {
            Status::Todo => "○",
            Status::Doing => "◐",
            Status::Done => "✓",
            Status::Cancelled => "✗",
        }
    }

    /// Sort order: in-progress first, then to-do, then finished.
    pub fn rank(self) -> u8 {
        match self {
            Status::Doing => 0,
            Status::Todo => 1,
            Status::Done => 2,
            Status::Cancelled => 3,
        }
    }

    pub fn is_open(self) -> bool {
        matches!(self, Status::Todo | Status::Doing)
    }

    pub fn parse(s: &str) -> Option<Status> {
        match s.trim().to_ascii_lowercase().as_str() {
            "todo" => Some(Status::Todo),
            "doing" => Some(Status::Doing),
            "done" => Some(Status::Done),
            "cancelled" | "canceled" => Some(Status::Cancelled),
            _ => None,
        }
    }
}

/// Stored as "A" | "B" | "C" (serde uses the variant name verbatim), kept as a
/// sort key while the UI shows coloured dots / words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    A,
    B,
    C,
}

impl Priority {
    pub fn dot(self) -> &'static str {
        match self {
            Priority::A => "🔴",
            Priority::B => "🟠",
            Priority::C => "🟡",
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Priority::A => 0,
            Priority::B => 1,
            Priority::C => 2,
        }
    }

    /// Accept A/B/C or high/medium/low (any case), matching the bash CLI.
    pub fn parse(s: &str) -> Option<Priority> {
        match s.trim().to_ascii_uppercase().as_str() {
            "A" | "HIGH" | "H" => Some(Priority::A),
            "B" | "MEDIUM" | "MED" | "M" => Some(Priority::B),
            "C" | "LOW" | "L" => Some(Priority::C),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub text: String,
    pub status: Status,
    #[serde(default)]
    pub priority: Option<Priority>,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub due: Option<String>,
    /// A task belongs to at most one category.
    #[serde(default)]
    pub category: Option<String>,
    /// Scheduled reminder time (RFC3339 local), when one is set.
    #[serde(default)]
    pub reminder: Option<String>,
}

impl Task {
    /// Grouping key: the category name, or "" for uncategorized.
    pub fn category_key(&self) -> &str {
        self.category.as_deref().unwrap_or("")
    }
}

/// Deserialization shim that tolerates two legacy shapes: items carrying
/// `done: bool` instead of `status`, and items carrying multi-value `tags`
/// instead of a single `category` (we adopt the first non-empty tag).
#[derive(Deserialize)]
struct RawTask {
    id: String,
    text: String,
    status: Option<Status>,
    #[serde(default)]
    done: Option<bool>,
    #[serde(default)]
    priority: Option<Priority>,
    #[serde(default)]
    created: String,
    #[serde(default)]
    due: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    reminder: Option<String>,
}

impl From<RawTask> for Task {
    fn from(r: RawTask) -> Task {
        let status = r.status.unwrap_or(if r.done == Some(true) {
            Status::Done
        } else {
            Status::Todo
        });
        let category = r
            .category
            .filter(|c| !c.trim().is_empty())
            .or_else(|| {
                r.tags
                    .unwrap_or_default()
                    .into_iter()
                    .find(|t| !t.trim().is_empty())
            });
        Task {
            id: r.id,
            text: r.text,
            status,
            priority: r.priority,
            created: r.created,
            due: r.due,
            category,
            reminder: r.reminder,
        }
    }
}

pub struct Store {
    pub path: PathBuf,
    pub tasks: Vec<Task>,
}

impl Store {
    pub fn resolve_path() -> PathBuf {
        if let Ok(p) = env::var("MONARCH_TODO_FILE") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
        let base = env::var("XDG_STATE_HOME")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                format!("{}/.local/state", env::var("HOME").unwrap_or_default())
            });
        PathBuf::from(base).join("monarch").join("todo.json")
    }

    pub fn load() -> Result<Store> {
        let path = Self::resolve_path();
        let tasks = if path.exists() {
            let data = fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            if data.trim().is_empty() {
                Vec::new()
            } else {
                let raw: Vec<RawTask> = serde_json::from_str(&data)
                    .with_context(|| format!("parsing {}", path.display()))?;
                raw.into_iter().map(Task::from).collect()
            }
        } else {
            Vec::new()
        };
        Ok(Store { path, tasks })
    }

    /// Atomic write (temp file + rename) so a concurrent reader — e.g. the bar
    /// binary or the bash CLI — never sees a half-written file. 2-space pretty
    /// JSON keeps diffs identical to jq's output.
    pub fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(&self.tasks)?;
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn add(
        &mut self,
        text: String,
        priority: Option<Priority>,
        due: Option<String>,
        category: Option<String>,
    ) -> &Task {
        let task = Task {
            id: new_id(),
            text,
            status: Status::Todo,
            priority,
            created: now_iso(),
            due,
            category,
            reminder: None,
        };
        self.tasks.push(task);
        self.tasks.last().unwrap()
    }

    /// Resolve a task by id prefix; Err on no/ambiguous match (like the bash CLI).
    pub fn find(&self, prefix: &str) -> std::result::Result<usize, String> {
        let matches: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.id.starts_with(prefix))
            .map(|(i, _)| i)
            .collect();
        match matches.len() {
            1 => Ok(matches[0]),
            0 => Err(format!("No task matches '{}'.", prefix)),
            n => Err(format!("'{}' matches {} tasks — be more specific.", prefix, n)),
        }
    }
}

/// Canonical ordering: status, then priority, then due date, then creation.
pub fn cmp_tasks(a: &Task, b: &Task) -> std::cmp::Ordering {
    let pa = a.priority.map(|p| p.rank()).unwrap_or(9);
    let pb = b.priority.map(|p| p.rank()).unwrap_or(9);
    a.status
        .rank()
        .cmp(&b.status.rank())
        .then(pa.cmp(&pb))
        .then_with(|| due_key(&a.due).cmp(due_key(&b.due)))
        .then_with(|| a.created.cmp(&b.created))
}

/// `None` due dates sort last.
pub fn due_key(due: &Option<String>) -> &str {
    due.as_deref().unwrap_or("9999-12-31")
}

/// Normalize a category input: trimmed, empty (or "-") → None (clears it).
pub fn normalize_category(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() || t == "-" {
        None
    } else {
        Some(t.to_string())
    }
}

fn new_id() -> String {
    format!("{:06x}", rand::random::<u32>() & 0x00ff_ffff)
}

fn now_iso() -> String {
    // Matches `date -Iseconds`, e.g. 2026-06-16T10:30:00+02:00
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}
