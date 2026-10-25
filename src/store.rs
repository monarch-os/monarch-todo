//! Shared todo store.
//!
//! Reads and writes the very same JSON file the `monarch todo` bash CLI uses,
//! so Noctalia and shell commands share one source of truth.
//! Path resolution mirrors the bash side exactly:
//!   $MONARCH_TODO_FILE  ||  ${XDG_STATE_HOME:-$HOME/.local/state}/monarch/todo.json

use crate::input::Recurrence;
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

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
    Critical,
    A,
    B,
    C,
}

impl Priority {
    pub fn dot(self) -> &'static str {
        match self {
            Priority::Critical => "‼",
            Priority::A => "🔴",
            Priority::B => "🟠",
            Priority::C => "🟡",
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Priority::Critical => 0,
            Priority::A => 1,
            Priority::B => 2,
            Priority::C => 3,
        }
    }

    /// Accept A/B/C or high/medium/low (any case), matching the bash CLI.
    pub fn parse(s: &str) -> Option<Priority> {
        match s.trim().to_ascii_uppercase().as_str() {
            "CRITICAL" | "CRITIQUE" | "P0" => Some(Priority::Critical),
            "A" | "HIGH" | "H" => Some(Priority::A),
            "B" | "MEDIUM" | "MED" | "M" | "NORMAL" => Some(Priority::B),
            "C" | "LOW" | "L" => Some(Priority::C),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
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
    /// One optional category per task.
    #[serde(default)]
    pub category: Option<String>,
    /// Scheduled reminder time (RFC3339 local), when one is set.
    #[serde(default)]
    pub reminder: Option<String>,
    /// Original cadence anchor when this occurrence's notification is snoozed.
    #[serde(default)]
    pub recurrence_anchor: Option<String>,
    #[serde(default)]
    pub recurrence: Option<Recurrence>,
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub next_id: Option<String>,
    #[serde(default)]
    pub notified: bool,
    /// Completed tracking segments, independent of the reminder schedule.
    #[serde(default)]
    pub time_spent_ms: u64,
    /// Wall-clock start of the current segment; persists across process/session restarts.
    #[serde(default)]
    pub started_at_ms: Option<i64>,
}

impl Task {
    /// Grouping key: the category name, or "" for uncategorized.
    pub fn category_key(&self) -> &str {
        self.category.as_deref().unwrap_or("")
    }

    pub fn elapsed_ms_at(&self, now_ms: i64) -> u64 {
        let running = if self.status == Status::Doing {
            self.started_at_ms
                .map(|start| now_ms.saturating_sub(start).max(0) as u64)
                .unwrap_or(0)
        } else {
            0
        };
        self.time_spent_ms.saturating_add(running)
    }

    fn track_status(&mut self, status: Status, now_ms: i64) {
        if status == Status::Doing {
            // Repeating 'start' must not reset a running segment. Legacy doing
            // tasks without a timestamp start counting only from this action.
            if self.status != Status::Doing || self.started_at_ms.is_none() {
                self.started_at_ms = Some(now_ms);
            }
        } else {
            self.time_spent_ms = self.elapsed_ms_at(now_ms);
            self.started_at_ms = None;
        }
        self.status = status;
    }
}

/// Deserialization shim that tolerates two legacy shapes: items carrying
/// `done: bool` instead of `status`, and items carrying multi-value `tags`
/// instead of a category. Experimental category arrays import their first name;
/// Store::load backs up the original multi-category data before any rewrite.
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
    categories: Option<Vec<String>>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    reminder: Option<String>,
    #[serde(default)]
    recurrence_anchor: Option<String>,
    #[serde(default)]
    recurrence: Option<Recurrence>,
    #[serde(default)]
    series: Option<String>,
    #[serde(default)]
    next_id: Option<String>,
    #[serde(default)]
    notified: bool,
    #[serde(default)]
    time_spent_ms: u64,
    #[serde(default)]
    started_at_ms: Option<i64>,
}

impl From<RawTask> for Task {
    fn from(r: RawTask) -> Task {
        let status = r.status.unwrap_or(if r.done == Some(true) {
            Status::Done
        } else {
            Status::Todo
        });
        let category = r
            .categories
            .unwrap_or_else(|| {
                r.category
                    .into_iter()
                    .chain(r.tags.unwrap_or_default())
                    .collect()
            })
            .into_iter()
            .find_map(|value| normalize_category(&value));
        Task {
            id: r.id,
            text: r.text,
            status,
            priority: r.priority,
            created: r.created,
            due: r.due,
            category,
            reminder: r.reminder,
            recurrence_anchor: r.recurrence_anchor,
            recurrence: r.recurrence,
            series: r.series,
            next_id: r.next_id,
            notified: r.notified,
            time_spent_ms: r.time_spent_ms,
            started_at_ms: r.started_at_ms,
        }
    }
}

// Undo snapshots use the same compatibility reader as the main store.
impl<'de> Deserialize<'de> for Task {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        RawTask::deserialize(deserializer).map(Task::from)
    }
}

pub struct Store {
    // A separate, stable inode protects the entire read/modify/rename transaction.
    _lock: fs::File,
    pub path: PathBuf,
    pub tasks: Vec<Task>,
}

#[derive(Serialize, Deserialize)]
struct UndoRecord {
    version: u32,
    at_ms: i64,
    changes: Vec<UndoChange>,
}

#[derive(Serialize, Deserialize)]
struct UndoChange {
    id: String,
    index: usize,
    before: Option<Task>,
    after: Option<Task>,
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
            .unwrap_or_else(|| format!("{}/.local/state", env::var("HOME").unwrap_or_default()));
        PathBuf::from(base).join("monarch").join("todo.json")
    }

    pub fn load() -> Result<Store> {
        let path = Self::resolve_path();
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path.with_extension("json.lock"))?;
        lock.lock_exclusive()?;
        let tasks = if path.exists() {
            let data =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            if data.trim().is_empty() {
                Vec::new()
            } else {
                let raw: Vec<RawTask> = serde_json::from_str(&data)
                    .with_context(|| format!("parsing {}", path.display()))?;
                if raw.iter().any(|task| {
                    task.categories
                        .as_ref()
                        .is_some_and(|names| names.len() > 1)
                        || task.tags.as_ref().is_some_and(|names| names.len() > 1)
                }) {
                    backup_once(&path, data.as_bytes())?;
                    let undo = path.with_extension("undo.json");
                    match fs::read(&undo) {
                        Ok(bytes) => backup_once(&undo, &bytes)?,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                raw.into_iter().map(Task::from).collect()
            }
        } else {
            Vec::new()
        };
        Ok(Store {
            path,
            tasks,
            _lock: lock,
        })
    }

    /// Atomic write (temp file + rename) so a concurrent reader — e.g. the bar
    /// binary or the bash CLI — never sees a half-written file. 2-space pretty
    /// JSON keeps diffs identical to jq's output.
    pub fn save(&self) -> Result<()> {
        write_json(&self.path, &self.tasks)
    }

    /// Journal only user mutations, not delivery/reconciliation. The journal is
    /// written first; after-image validation makes a crash before save harmless.
    pub fn save_user_change(&self, before: &[Task]) -> Result<()> {
        let mut ids = HashSet::new();
        for task in before {
            if !ids.insert(task.id.as_str()) {
                bail!("Duplicate task IDs prevent safe undo");
            }
        }
        let after: HashMap<&str, &Task> = self
            .tasks
            .iter()
            .map(|task| (task.id.as_str(), task))
            .collect();
        if after.len() != self.tasks.len() {
            bail!("Duplicate task IDs prevent safe undo");
        }
        let mut changes = Vec::new();
        for (index, task) in before.iter().enumerate() {
            let current = after.get(task.id.as_str()).copied();
            if current != Some(task) {
                changes.push(UndoChange {
                    id: task.id.clone(),
                    index,
                    before: Some(task.clone()),
                    after: current.cloned(),
                });
            }
        }
        for (index, task) in self.tasks.iter().enumerate() {
            if !ids.contains(task.id.as_str()) {
                changes.push(UndoChange {
                    id: task.id.clone(),
                    index,
                    before: None,
                    after: Some(task.clone()),
                });
            }
        }
        if changes.is_empty() {
            return Ok(());
        }
        write_json(
            &self.path.with_extension("undo.json"),
            &UndoRecord {
                version: 1,
                at_ms: chrono::Local::now().timestamp_millis(),
                changes,
            },
        )?;
        self.save()
    }

    pub fn undo(&mut self) -> Result<()> {
        let path = self.path.with_extension("undo.json");
        let bytes = fs::read(&path).context("No action to undo")?;
        let raw: serde_json::Value =
            serde_json::from_slice(&bytes).context("Invalid undo record")?;
        if raw["changes"].as_array().is_some_and(|changes| {
            changes.iter().any(|change| {
                ["before", "after"].iter().any(|field| {
                    ["categories", "tags"].iter().any(|key| {
                        change[*field][*key]
                            .as_array()
                            .is_some_and(|names| names.len() > 1)
                    })
                })
            })
        }) {
            backup_once(&path, &bytes)?;
        }
        let record: UndoRecord = serde_json::from_value(raw).context("Invalid undo record")?;
        if record.version != 1 || record.changes.is_empty() {
            bail!("Unsupported undo record");
        }
        // Validate the entire patch before touching anything. An automatic
        // delivery or external edit to a touched task must never be overwritten.
        let mut seen = HashSet::new();
        // Index once instead of rescanning/allocating a vector per changed task
        // when undoing a bulk clear. Duplicate IDs remain an explicit conflict.
        let mut current = HashMap::new();
        let mut duplicates = HashSet::new();
        for task in &self.tasks {
            if current.insert(task.id.as_str(), task).is_some() {
                duplicates.insert(task.id.as_str());
            }
        }
        for change in &record.changes {
            if (change.before.is_none() && change.after.is_none())
                || !seen.insert(&change.id)
                || change
                    .before
                    .as_ref()
                    .is_some_and(|task| task.id != change.id)
                || change
                    .after
                    .as_ref()
                    .is_some_and(|task| task.id != change.id)
            {
                bail!("Invalid undo record");
            }
            let matches = !duplicates.contains(change.id.as_str())
                && current.get(change.id.as_str()).copied() == change.after.as_ref();
            if !matches {
                bail!("Cannot undo: an affected task changed since the last action");
            }
        }
        let now_ms = chrono::Local::now().timestamp_millis();
        for change in record.changes {
            let index = self.tasks.iter().position(|task| task.id == change.id);
            if let Some(mut task) = change.before {
                // Resume a previously running task from now, excluding the time
                // it spent completed, paused or deleted. Pure metadata edits do
                // not restart an existing tracking segment.
                if task.status == Status::Doing
                    && task.started_at_ms.is_some()
                    && change
                        .after
                        .as_ref()
                        .is_none_or(|after| after.status != Status::Doing)
                {
                    task.time_spent_ms = change
                        .after
                        .as_ref()
                        .map(|after| after.time_spent_ms)
                        .unwrap_or_else(|| task.elapsed_ms_at(record.at_ms));
                    task.started_at_ms = Some(now_ms);
                }
                if let Some(index) = index {
                    self.tasks[index] = task;
                } else {
                    self.tasks.insert(change.index.min(self.tasks.len()), task);
                }
            } else if let Some(index) = index {
                self.tasks.remove(index);
            }
        }
        self.save()?;
        fs::remove_file(path)
            .context("Tasks restored, but the undo record could not be cleared")?;
        Ok(())
    }

    pub fn snooze(&mut self, i: usize, when: &str) -> Result<()> {
        let task = &mut self.tasks[i];
        if !task.status.is_open() {
            bail!("Reopen the task before snoozing its reminder");
        }
        let date = crate::input::parse_date(when, chrono::Local::now())?;
        if date <= chrono::Local::now() {
            bail!("The scheduled time is in the past");
        }
        if task.recurrence.is_some() && task.recurrence_anchor.is_none() {
            task.recurrence_anchor = task.reminder.clone();
        }
        task.reminder = Some(date.to_rfc3339());
        task.notified = false;
        Ok(())
    }

    pub fn add(
        &mut self,
        text: String,
        priority: Option<Priority>,
        due: Option<String>,
        category: Option<String>,
    ) -> &mut Task {
        let category = category.as_deref().and_then(normalize_category);
        let task = Task {
            id: self.unique_id(),
            text,
            status: Status::Todo,
            priority,
            created: now_iso(),
            due,
            category,
            reminder: None,
            recurrence_anchor: None,
            recurrence: None,
            series: None,
            next_id: None,
            notified: false,
            time_spent_ms: 0,
            started_at_ms: None,
        };
        self.tasks.push(task);
        self.tasks.last_mut().unwrap()
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
            n => Err(format!(
                "'{}' matches {} tasks — be more specific.",
                prefix, n
            )),
        }
    }

    /// Complete once: a recurring occurrence produces exactly one successor.
    /// Reopening an old occurrence must not duplicate an existing successor.
    pub fn set_status(&mut self, i: usize, status: Status) -> Result<()> {
        let now = chrono::Local::now();
        if status == Status::Done && self.tasks[i].next_id.is_none() {
            if let Some(rule) = self.tasks[i].recurrence.clone() {
                let current = self.tasks[i]
                    .recurrence_anchor
                    .as_deref()
                    .or(self.tasks[i].reminder.as_deref())
                    .ok_or_else(|| anyhow::anyhow!("Recurring task has no scheduled time"))?;
                let base =
                    chrono::DateTime::parse_from_rfc3339(current)?.with_timezone(&chrono::Local);
                let next_time = rule.next_after(base, now)?;
                let mut next = self.tasks[i].clone();
                next.id = self.unique_id();
                next.series = Some(next.series.unwrap_or_else(|| self.tasks[i].id.clone()));
                next.next_id = None;
                next.notified = false;
                next.status = Status::Todo;
                next.time_spent_ms = 0;
                next.started_at_ms = None;
                next.recurrence_anchor = None;
                next.created = now_iso();
                next.reminder = Some(next_time.to_rfc3339());
                next.due = Some(next_time.format("%Y-%m-%d").to_string());
                self.tasks[i].next_id = Some(next.id.clone());
                self.tasks.push(next);
            }
        }
        self.tasks[i].track_status(status, now.timestamp_millis());
        Ok(())
    }

    fn unique_id(&self) -> String {
        loop {
            let id = new_id();
            if !self.tasks.iter().any(|task| task.id == id) {
                return id;
            }
        }
    }
}

fn backup_once(path: &Path, bytes: &[u8]) -> Result<()> {
    for number in 0..10000 {
        let suffix = if number == 0 {
            "before-single-category.json".to_owned()
        } else {
            format!("before-single-category-{number}.json")
        };
        let backup = path.with_extension(suffix);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&backup)
        {
            Ok(mut file) => {
                let result = (|| -> Result<()> {
                    file.write_all(bytes)?;
                    file.sync_all()?;
                    let parent = path
                        .parent()
                        .filter(|dir| !dir.as_os_str().is_empty())
                        .unwrap_or_else(|| Path::new("."));
                    fs::File::open(parent)?.sync_all()?;
                    Ok(())
                })();
                if result.is_err() {
                    let _ = fs::remove_file(&backup);
                }
                return result;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::read(&backup)? == bytes {
                    return Ok(());
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Backing up {} before single-category conversion",
                        path.display()
                    )
                })
            }
        }
    }
    bail!("Too many single-category backup files; refusing to discard category data")
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(value)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Canonical ordering: priority, then status, then due date, then creation.
pub fn cmp_tasks(a: &Task, b: &Task) -> std::cmp::Ordering {
    let pa = a.priority.map(|p| p.rank()).unwrap_or(Priority::B.rank());
    let pb = b.priority.map(|p| p.rank()).unwrap_or(Priority::B.rank());
    pa.cmp(&pb)
        .then(a.status.rank().cmp(&b.status.rank()))
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
    chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

#[cfg(test)]
mod tracking_tests {
    use super::*;

    fn task() -> Task {
        serde_json::from_str(r#"{"id":"test","text":"Task","status":"todo"}"#).unwrap()
    }

    #[test]
    fn start_pause_resume_complete_accumulates_once() {
        let mut task = task();
        task.track_status(Status::Doing, 1_000);
        task.track_status(Status::Doing, 2_000);
        assert_eq!(task.started_at_ms, Some(1_000));
        assert_eq!(task.elapsed_ms_at(3_500), 2_500);
        task.track_status(Status::Todo, 4_000);
        assert_eq!(task.time_spent_ms, 3_000);
        assert_eq!(task.elapsed_ms_at(10_000), 3_000);
        assert_eq!(task.started_at_ms, None);
        task.track_status(Status::Doing, 20_000);
        task.track_status(Status::Done, 25_000);
        assert_eq!(task.time_spent_ms, 8_000);
        task.track_status(Status::Done, 30_000);
        task.track_status(Status::Todo, 40_000);
        assert_eq!(task.time_spent_ms, 8_000);
        assert_eq!(task.started_at_ms, None);
    }

    #[test]
    fn cancellation_stops_tracking_and_serialization_preserves_it() {
        let mut task = task();
        task.track_status(Status::Doing, 1_000);
        let json = serde_json::to_string(&task).unwrap();
        let mut task: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(task.elapsed_ms_at(9_000), 8_000);
        task.track_status(Status::Cancelled, 10_000);
        assert_eq!(task.time_spent_ms, 9_000);
        assert_eq!(task.elapsed_ms_at(50_000), 9_000);
    }

    #[test]
    fn old_doing_tasks_have_no_fabricated_history() {
        let mut task = task();
        task.status = Status::Doing;
        assert_eq!(task.elapsed_ms_at(100_000), 0);
        task.track_status(Status::Doing, 100_000);
        assert_eq!(task.elapsed_ms_at(101_500), 1_500);
    }

    #[test]
    fn clock_rollback_never_subtracts_accumulated_time() {
        let mut task = task();
        task.time_spent_ms = 5_000;
        task.track_status(Status::Doing, 10_000);
        assert_eq!(task.elapsed_ms_at(9_000), 5_000);
        task.track_status(Status::Todo, 8_000);
        assert_eq!(task.time_spent_ms, 5_000);
    }
}
