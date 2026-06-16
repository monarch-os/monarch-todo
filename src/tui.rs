//! Terminal UI for monarch-todo.
//!
//! Fast, keyboard-driven front-end over the shared JSON store. Tasks are grouped
//! by category into foldable sections. Designed to run in a small floating
//! terminal (sized via a Niri window-rule) but the layout is responsive.

use crate::store::{normalize_category, Priority, Status, Store, Task};
use anyhow::Result;
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::collections::HashSet;
use std::io;
use std::process::Command;

pub fn run() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new()?;
    let result = app.main_loop(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

const UNCATEGORIZED: &str = "Uncategorized";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Open,
    All,
    Done,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Scope::Open => "open",
            Scope::All => "all",
            Scope::Done => "done",
        }
    }
    fn next(self) -> Scope {
        match self {
            Scope::Open => Scope::All,
            Scope::All => Scope::Done,
            Scope::Done => Scope::Open,
        }
    }
}

/// A visible line: either a category header or a task (store index).
enum Row {
    Header { key: String, count: usize, folded: bool },
    Task(usize),
}

#[derive(Clone, Copy)]
enum InputKind {
    Add,
    EditText,
    EditCategory,
    ReminderCustom,
}

impl InputKind {
    fn prompt(self) -> &'static str {
        match self {
            InputKind::Add => "New task",
            InputKind::EditText => "Edit text",
            InputKind::EditCategory => "Category (empty to clear)",
            InputKind::ReminderCustom => "Remind in (e.g. 90m, 2d, 3w, 1h30)",
        }
    }
}

#[derive(Clone)]
enum ReminderChoice {
    /// Minutes from now.
    In(i64),
    /// Open the free-form duration input.
    Custom,
}

#[derive(Clone)]
enum CategoryChoice {
    /// Set (or clear, when None) the category directly.
    Set(Option<String>),
    /// Open the free-text input to create a new category.
    New,
}

/// Priority picker entries (label, value).
const PRIORITIES: [(&str, Option<Priority>); 4] = [
    ("🔴  High", Some(Priority::A)),
    ("🟠  Medium", Some(Priority::B)),
    ("🟡  Low", Some(Priority::C)),
    ("—  None", None),
];

enum Mode {
    Normal,
    Input {
        kind: InputKind,
        buffer: String,
        target: Option<usize>,
    },
    Filter,
    Priority {
        target: usize,
        state: ListState,
    },
    Category {
        target: usize,
        options: Vec<(String, CategoryChoice)>,
        state: ListState,
    },
    Reminder {
        target: usize,
        options: Vec<(String, ReminderChoice)>,
        state: ListState,
    },
    Confirm {
        target: usize,
    },
    Help,
}

struct App {
    store: Store,
    scope: Scope,
    title_filter: String,
    folded: HashSet<String>,
    rows: Vec<Row>,
    list: ListState,
    mode: Mode,
    status: String,
    quit: bool,
}

impl App {
    fn new() -> Result<App> {
        let mut store = Store::load()?;
        // Drop reminders whose time has passed so the UI only shows live ones.
        let now = Local::now();
        let mut changed = false;
        for t in &mut store.tasks {
            if t.reminder.as_deref().and_then(parse_reminder).is_some_and(|dt| dt <= now) {
                t.reminder = None;
                changed = true;
            }
        }
        if changed {
            let _ = store.save();
        }
        let mut app = App {
            store,
            scope: Scope::Open,
            title_filter: String::new(),
            folded: HashSet::new(),
            rows: Vec::new(),
            list: ListState::default(),
            mode: Mode::Normal,
            status: String::new(),
            quit: false,
        };
        app.rebuild();
        Ok(app)
    }

    /// Recompute the grouped, sorted, folded set of rows; keep a valid selection.
    fn rebuild(&mut self) {
        let needle = self.title_filter.to_lowercase();
        let idxs: Vec<usize> = self
            .store
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| match self.scope {
                Scope::Open => t.status.is_open(),
                Scope::All => true,
                Scope::Done => !t.status.is_open(),
            })
            .filter(|(_, t)| needle.is_empty() || t.text.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();

        // Distinct category keys, named first (alphabetical), uncategorized last.
        let mut keys: Vec<String> = idxs
            .iter()
            .map(|&i| self.store.tasks[i].category_key().to_string())
            .collect();
        keys.sort();
        keys.dedup();
        keys.sort_by(|a, b| group_rank(a).cmp(&group_rank(b)).then_with(|| a.cmp(b)));

        let mut rows = Vec::new();
        for key in keys {
            let mut group: Vec<usize> = idxs
                .iter()
                .copied()
                .filter(|&i| self.store.tasks[i].category_key() == key)
                .collect();
            group.sort_by(|&a, &b| crate::store::cmp_tasks(&self.store.tasks[a], &self.store.tasks[b]));
            let folded = self.folded.contains(&key);
            rows.push(Row::Header {
                key: key.clone(),
                count: group.len(),
                folded,
            });
            if !folded {
                rows.extend(group.into_iter().map(Row::Task));
            }
        }

        self.rows = rows;
        if self.rows.is_empty() {
            self.list.select(None);
        } else {
            let sel = self.list.selected().unwrap_or(0).min(self.rows.len() - 1);
            self.list.select(Some(sel));
        }
    }

    fn current(&self) -> Option<usize> {
        match self.list.selected().and_then(|i| self.rows.get(i)) {
            Some(Row::Task(i)) => Some(*i),
            _ => None,
        }
    }

    fn current_header_key(&self) -> Option<String> {
        match self.list.selected().and_then(|i| self.rows.get(i)) {
            Some(Row::Header { key, .. }) => Some(key.clone()),
            _ => None,
        }
    }

    /// Category context of the current row (for "add inherits the section").
    fn current_category(&self) -> Option<String> {
        match self.list.selected().and_then(|i| self.rows.get(i)) {
            Some(Row::Header { key, .. }) => normalize_category(key),
            Some(Row::Task(i)) => self.store.tasks[*i].category.clone(),
            None => None,
        }
    }

    fn all_keys(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter_map(|r| match r {
                Row::Header { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect()
    }

    fn persist(&mut self) {
        if let Err(e) = self.store.save() {
            self.status = format!("save failed: {}", e);
        }
    }

    fn main_loop<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        while !self.quit {
            terminal.draw(|f| self.draw(f))?;
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                self.on_key(key.code);
            }
        }
        Ok(())
    }

    fn on_key(&mut self, code: KeyCode) {
        match &mut self.mode {
            Mode::Input { .. } => return self.key_input(code),
            Mode::Filter => return self.key_filter(code),
            Mode::Priority { .. } => return self.key_priority(code),
            Mode::Category { .. } => return self.key_category(code),
            Mode::Reminder { .. } => return self.key_reminder(code),
            Mode::Confirm { .. } => return self.key_confirm(code),
            Mode::Help => {
                self.mode = Mode::Normal;
                return;
            }
            Mode::Normal => {}
        }

        self.status.clear();
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if !self.title_filter.is_empty() {
                    self.title_filter.clear();
                    self.rebuild();
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Char('g') => self.list.select((!self.rows.is_empty()).then_some(0)),
            KeyCode::Char('G') => {
                self.list
                    .select((!self.rows.is_empty()).then_some(self.rows.len() - 1));
            }
            // Enter / Space: fold a header, or toggle done on a task.
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(key) = self.current_header_key() {
                    self.toggle_fold(&key);
                } else {
                    self.toggle_done();
                }
            }
            KeyCode::Left => {
                if let Some(key) = self.current_header_key() {
                    self.folded.insert(key);
                    self.rebuild();
                }
            }
            KeyCode::Right => {
                if let Some(key) = self.current_header_key() {
                    self.folded.remove(&key);
                    self.rebuild();
                }
            }
            KeyCode::Char('Z') => self.toggle_fold_all(),
            KeyCode::Char('s') => self.set_status(Status::Doing),
            KeyCode::Char('x') => self.set_status(Status::Cancelled),
            KeyCode::Char('o') => self.set_status(Status::Todo),
            KeyCode::Char('a') => {
                self.mode = Mode::Input {
                    kind: InputKind::Add,
                    buffer: String::new(),
                    target: None,
                };
            }
            KeyCode::Char('e') => self.begin_edit(InputKind::EditText),
            KeyCode::Char('c') => self.open_category(),
            KeyCode::Char('m') => self.open_reminder(),
            KeyCode::Char('p') => self.open_priority(),
            KeyCode::Char('X') | KeyCode::Delete => {
                if let Some(idx) = self.current() {
                    self.mode = Mode::Confirm { target: idx };
                }
            }
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Tab => {
                self.scope = self.scope.next();
                self.rebuild();
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
    }

    fn move_sel(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as i32;
        let cur = self.list.selected().unwrap_or(0) as i32;
        self.list.select(Some((cur + delta).rem_euclid(len) as usize));
    }

    fn toggle_fold(&mut self, key: &str) {
        if self.folded.contains(key) {
            self.folded.remove(key);
        } else {
            self.folded.insert(key.to_string());
        }
        self.rebuild();
    }

    fn toggle_fold_all(&mut self) {
        let keys = self.all_keys();
        let any_open = keys.iter().any(|k| !self.folded.contains(k));
        if any_open {
            self.folded.extend(keys);
        } else {
            self.folded.clear();
        }
        self.rebuild();
    }

    /// Mutate the selected task, persist, rebuild, and keep it selected even if
    /// it moved between groups.
    fn mutate_selected<F: FnOnce(&mut Task)>(&mut self, f: F) {
        if let Some(idx) = self.current() {
            let id = self.store.tasks[idx].id.clone();
            f(&mut self.store.tasks[idx]);
            self.persist();
            self.rebuild();
            self.select_task(&id);
        }
    }

    fn select_task(&mut self, id: &str) {
        if let Some(pos) = self.rows.iter().position(
            |r| matches!(r, Row::Task(i) if self.store.tasks[*i].id == id),
        ) {
            self.list.select(Some(pos));
        }
    }

    fn set_status(&mut self, status: Status) {
        self.mutate_selected(|t| t.status = status);
    }

    fn toggle_done(&mut self) {
        self.mutate_selected(|t| {
            t.status = if t.status == Status::Done {
                Status::Todo
            } else {
                Status::Done
            };
        });
    }

    fn begin_edit(&mut self, kind: InputKind) {
        let Some(idx) = self.current() else { return };
        let buffer = match kind {
            InputKind::EditText => self.store.tasks[idx].text.clone(),
            InputKind::EditCategory => self.store.tasks[idx].category.clone().unwrap_or_default(),
            _ => String::new(),
        };
        self.mode = Mode::Input {
            kind,
            buffer,
            target: Some(idx),
        };
    }

    fn open_reminder(&mut self) {
        let Some(idx) = self.current() else { return };
        let mut state = ListState::default();
        state.select(Some(0));
        self.mode = Mode::Reminder {
            target: idx,
            options: reminder_presets(&self.store.tasks[idx]),
            state,
        };
    }

    fn open_priority(&mut self) {
        let Some(idx) = self.current() else { return };
        let cur = self.store.tasks[idx].priority;
        let sel = PRIORITIES.iter().position(|(_, p)| *p == cur).unwrap_or(0);
        let mut state = ListState::default();
        state.select(Some(sel));
        self.mode = Mode::Priority { target: idx, state };
    }

    fn distinct_categories(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .store
            .tasks
            .iter()
            .filter_map(|t| t.category.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    fn open_category(&mut self) {
        let Some(idx) = self.current() else { return };
        let cats = self.distinct_categories();
        let current = self.store.tasks[idx].category.clone();

        let mut options = vec![("(no category)".to_string(), CategoryChoice::Set(None))];
        options.extend(
            cats.iter()
                .map(|c| (c.clone(), CategoryChoice::Set(Some(c.clone())))),
        );
        options.push(("＋ New category…".to_string(), CategoryChoice::New));

        let sel = match &current {
            None => 0,
            Some(c) => cats.iter().position(|x| x == c).map_or(0, |p| p + 1),
        };
        let mut state = ListState::default();
        state.select(Some(sel));
        self.mode = Mode::Category {
            target: idx,
            options,
            state,
        };
    }

    fn key_input(&mut self, code: KeyCode) {
        let Mode::Input {
            kind,
            buffer,
            target,
        } = &mut self.mode
        else {
            return;
        };
        match code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Char(c) => buffer.push(c),
            KeyCode::Enter => {
                let kind = *kind;
                let target = *target;
                let value = buffer.trim().to_string();
                self.commit_input(kind, target, value);
                self.mode = Mode::Normal;
            }
            _ => {}
        }
    }

    fn commit_input(&mut self, kind: InputKind, target: Option<usize>, value: String) {
        match kind {
            InputKind::Add => {
                if !value.is_empty() {
                    let category = self.current_category();
                    let id = self.store.add(value, None, None, category).id.clone();
                    self.persist();
                    self.rebuild();
                    self.select_task(&id);
                }
            }
            InputKind::EditText => {
                if let (Some(idx), false) = (target, value.is_empty()) {
                    let id = self.store.tasks[idx].id.clone();
                    self.store.tasks[idx].text = value;
                    self.persist();
                    self.rebuild();
                    self.select_task(&id);
                }
            }
            InputKind::EditCategory => {
                if let Some(idx) = target {
                    let id = self.store.tasks[idx].id.clone();
                    self.store.tasks[idx].category = normalize_category(&value);
                    self.persist();
                    self.rebuild();
                    self.select_task(&id);
                }
            }
            InputKind::ReminderCustom => {
                if let Some(idx) = target {
                    match parse_duration_minutes(&value) {
                        Some(m) => self.schedule_reminder(idx, m),
                        None => self.status = "reminder: try 90m, 2d, 3w or 1h30".into(),
                    }
                }
            }
        }
    }

    fn schedule_reminder(&mut self, idx: usize, minutes: i64) {
        if minutes <= 0 {
            self.status = "reminder: that time is in the past".into();
            return;
        }
        let text = self.store.tasks[idx].text.clone();
        // Match the notification urgency to the task's priority.
        let urgency = match self.store.tasks[idx].priority {
            Some(Priority::A) => "critical",
            Some(Priority::B) => "normal",
            Some(Priority::C) => "low",
            None => "normal",
        };
        match Command::new("monarch-reminder")
            .arg(minutes.to_string())
            .arg("-u")
            .arg(urgency)
            .arg(&text)
            .spawn()
        {
            Ok(_) => {
                let target = Local::now() + Duration::minutes(minutes);
                let id = self.store.tasks[idx].id.clone();
                self.store.tasks[idx].reminder =
                    Some(target.format("%Y-%m-%dT%H:%M:%S%:z").to_string());
                self.persist();
                self.rebuild();
                self.select_task(&id);
                self.status = format!("🔔 reminder in {}", human_minutes(minutes));
            }
            Err(e) => self.status = format!("reminder failed: {}", e),
        }
    }

    fn key_filter(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.title_filter.clear();
                self.mode = Mode::Normal;
                self.rebuild();
            }
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                self.title_filter.pop();
                self.rebuild();
            }
            KeyCode::Char(c) => {
                self.title_filter.push(c);
                self.rebuild();
            }
            _ => {}
        }
    }

    fn key_priority(&mut self, code: KeyCode) {
        let Mode::Priority { target, state } = &mut self.mode else {
            return;
        };
        let target = *target;
        // Navigable list + direct hotkeys. `chosen` = Some(value) to apply.
        let chosen: Option<Option<Priority>> = match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let i = (state.selected().unwrap_or(0) + 1) % PRIORITIES.len();
                state.select(Some(i));
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let i = (state.selected().unwrap_or(0) + PRIORITIES.len() - 1) % PRIORITIES.len();
                state.select(Some(i));
                None
            }
            KeyCode::Enter => Some(PRIORITIES[state.selected().unwrap_or(0)].1),
            KeyCode::Char('1') | KeyCode::Char('a') => Some(Some(Priority::A)),
            KeyCode::Char('2') | KeyCode::Char('b') => Some(Some(Priority::B)),
            KeyCode::Char('3') => Some(Some(Priority::C)),
            KeyCode::Char('0') | KeyCode::Char('n') => Some(None),
            _ => None,
        };
        if let Some(priority) = chosen {
            let id = self.store.tasks[target].id.clone();
            self.store.tasks[target].priority = priority;
            self.persist();
            self.rebuild();
            self.select_task(&id);
            self.mode = Mode::Normal;
        }
    }

    fn key_category(&mut self, code: KeyCode) {
        let Mode::Category {
            target,
            options,
            state,
        } = &mut self.mode
        else {
            return;
        };
        let total = options.len();
        match code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Down | KeyCode::Char('j') => {
                let i = (state.selected().unwrap_or(0) + 1) % total;
                state.select(Some(i));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let i = (state.selected().unwrap_or(0) + total - 1) % total;
                state.select(Some(i));
            }
            KeyCode::Enter => {
                let target = *target;
                match options[state.selected().unwrap_or(0)].1.clone() {
                    CategoryChoice::Set(cat) => {
                        let id = self.store.tasks[target].id.clone();
                        self.store.tasks[target].category = cat;
                        self.persist();
                        self.rebuild();
                        self.select_task(&id);
                        self.mode = Mode::Normal;
                    }
                    CategoryChoice::New => {
                        self.mode = Mode::Input {
                            kind: InputKind::EditCategory,
                            buffer: String::new(),
                            target: Some(target),
                        };
                    }
                }
            }
            _ => {}
        }
    }

    fn key_reminder(&mut self, code: KeyCode) {
        let Mode::Reminder {
            target,
            options,
            state,
        } = &mut self.mode
        else {
            return;
        };
        let total = options.len();
        match code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Down | KeyCode::Char('j') => {
                let i = (state.selected().unwrap_or(0) + 1) % total;
                state.select(Some(i));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let i = (state.selected().unwrap_or(0) + total - 1) % total;
                state.select(Some(i));
            }
            KeyCode::Enter => {
                let target = *target;
                let choice = options[state.selected().unwrap_or(0)].1.clone();
                match choice {
                    ReminderChoice::In(m) => {
                        self.schedule_reminder(target, m);
                        self.mode = Mode::Normal;
                    }
                    ReminderChoice::Custom => {
                        self.mode = Mode::Input {
                            kind: InputKind::ReminderCustom,
                            buffer: String::new(),
                            target: Some(target),
                        };
                    }
                }
            }
            _ => {}
        }
    }

    fn key_confirm(&mut self, code: KeyCode) {
        let Mode::Confirm { target } = self.mode else {
            return;
        };
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.store.tasks.remove(target);
                self.persist();
                self.rebuild();
                self.mode = Mode::Normal;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.mode = Mode::Normal,
            _ => {}
        }
    }

    // ---- rendering ----------------------------------------------------------

    fn draw(&mut self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(f.size());

        self.draw_header(f, chunks[0]);
        self.draw_list(f, chunks[1]);
        self.draw_footer(f, chunks[2]);

        match &self.mode {
            Mode::Input { kind, buffer, .. } => self.draw_input(f, kind.prompt(), buffer),
            Mode::Priority { state, .. } => {
                let mut state = state.clone();
                self.draw_picker(f, " Priority ", PRIORITIES.iter().map(|(l, _)| *l), &mut state, 30);
            }
            Mode::Category { options, state, .. } => {
                let opts: Vec<String> = options.iter().map(|(l, _)| l.clone()).collect();
                let mut state = state.clone();
                self.draw_picker(f, " Category ", opts.iter().map(String::as_str), &mut state, 40);
            }
            Mode::Reminder { options, state, .. } => {
                let opts: Vec<String> = options.iter().map(|(l, _)| l.clone()).collect();
                let mut state = state.clone();
                self.draw_picker(f, " Remind me… ", opts.iter().map(String::as_str), &mut state, 44);
            }
            Mode::Confirm { target } => {
                let text = self.store.tasks[*target].text.clone();
                self.draw_confirm(f, &text);
            }
            Mode::Help => self.draw_help(f),
            _ => {}
        }
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let mut spans = vec![
            Span::styled(" Todo ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(" {} ", self.scope.label()),
                Style::default().fg(Color::Black).bg(Color::Cyan),
            ),
        ];
        if !self.title_filter.is_empty() {
            spans.push(Span::styled(
                format!(" /{} ", self.title_filter),
                Style::default().fg(Color::Yellow),
            ));
        }
        let shown = self.rows.iter().filter(|r| matches!(r, Row::Task(_))).count();
        spans.push(Span::styled(
            format!("  {} shown · {} total", shown, self.store.tasks.len()),
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        if self.rows.is_empty() {
            let msg = if self.store.tasks.is_empty() {
                "No tasks yet — press 'a' to add one."
            } else {
                "Nothing matches the current view."
            };
            f.render_widget(
                Paragraph::new(msg)
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(Alignment::Center),
                area,
            );
            return;
        }

        let now = Local::now();
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| match row {
                Row::Header { key, count, folded } => header_item(key, *count, *folded),
                Row::Task(i) => task_item(&self.store.tasks[*i], now),
            })
            .collect();

        let list = List::new(items)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▌");
        f.render_stateful_widget(list, area, &mut self.list);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let text = if !self.status.is_empty() {
            self.status.clone()
        } else {
            "␣/⏎ done·fold · s start · x cancel · o reopen · a add · e edit · c categ · p prio · m remind · X del · / find · ←/→/Z fold · ⇥ scope · ? · q".into()
        };
        f.render_widget(
            Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }

    fn draw_input(&self, f: &mut Frame, prompt: &str, buffer: &str) {
        let area = popup(f.size(), 60, 3);
        f.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", prompt));
        f.render_widget(Paragraph::new(format!("{}▏", buffer)).block(block), area);
    }

    /// Generic centered list picker used by the priority/category/reminder overlays.
    fn draw_picker<'a>(
        &self,
        f: &mut Frame,
        title: &str,
        items: impl Iterator<Item = &'a str>,
        state: &mut ListState,
        width: u16,
    ) {
        let items: Vec<ListItem> = items.map(ListItem::new).collect();
        let h = (items.len() as u16 + 2).min(16);
        let area = popup(f.size(), width, h);
        f.render_widget(Clear, area);
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title.to_string()),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▌ ");
        f.render_stateful_widget(list, area, state);
    }

    fn draw_confirm(&self, f: &mut Frame, text: &str) {
        let area = popup(f.size(), 50, 4);
        f.render_widget(Clear, area);
        let body = Paragraph::new(vec![
            Line::from(format!("Delete \"{}\"?", truncate(text, 40))),
            Line::from(Span::styled(
                "y / Enter = delete   ·   n / Esc = cancel",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title(" Confirm "));
        f.render_widget(body, area);
    }

    fn draw_help(&self, f: &mut Frame) {
        let area = popup(f.size(), 56, 18);
        f.render_widget(Clear, area);
        let lines = vec![
            Line::from("Navigate     ↑/k ↓/j   g top   G bottom"),
            Line::from("On a task    ␣/⏎ done   s start   x cancel   o reopen"),
            Line::from("On a header  ␣/⏎ fold   ← fold   → unfold   Z fold all"),
            Line::from("Edit         a add   e text   c category   p priority"),
            Line::from("Reminder     m  → presets (hours…weeks) or custom 2d/3w"),
            Line::from("Delete       X / Delete   (asks to confirm)"),
            Line::from("View         / find   ⇥ scope (open/all/done)"),
            Line::from("Other        ? help   q/Esc quit (clears find first)"),
            Line::from(""),
            Line::from(Span::styled(
                "press any key to close",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        f.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Help ")),
            area,
        );
    }
}

fn header_item(key: &str, count: usize, folded: bool) -> ListItem<'static> {
    let name = if key.is_empty() { UNCATEGORIZED } else { key };
    let arrow = if folded { "▸" } else { "▾" };
    ListItem::new(Line::from(vec![
        Span::styled(
            format!("{} {}", arrow, name),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  ({})", count), Style::default().fg(Color::DarkGray)),
    ]))
}

fn task_item(t: &Task, now: DateTime<Local>) -> ListItem<'static> {
    let (marker_color, dim) = match t.status {
        Status::Todo => (Color::Gray, false),
        Status::Doing => (Color::Yellow, false),
        Status::Done => (Color::Green, true),
        Status::Cancelled => (Color::Red, true),
    };

    let mut text_style = Style::default();
    if dim {
        text_style = text_style
            .add_modifier(Modifier::DIM)
            .add_modifier(Modifier::CROSSED_OUT);
    }

    // Two-space indent so tasks sit under their category header.
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(t.status.marker(), Style::default().fg(marker_color)),
        Span::raw(" "),
    ];
    if let Some(p) = t.priority {
        spans.push(Span::raw(format!("{} ", p.dot())));
    }
    spans.push(Span::styled(t.text.clone(), text_style));
    if let Some(due) = &t.due {
        spans.push(Span::styled(
            format!("  ⏰{}", due),
            Style::default().fg(Color::Blue),
        ));
    }
    if let Some(mins) = t
        .reminder
        .as_deref()
        .and_then(parse_reminder)
        .map(|dt| (dt - now).num_minutes())
        .filter(|m| *m > 0)
    {
        spans.push(Span::styled(
            format!("  🔔{}", human_minutes(mins)),
            Style::default().fg(Color::Magenta),
        ));
    }
    ListItem::new(Line::from(spans))
}

/// Parse a stored reminder timestamp (RFC3339) into local time.
fn parse_reminder(s: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Local))
}

/// Uncategorized (key "") sorts after named categories.
fn group_rank(key: &str) -> u8 {
    if key.is_empty() {
        1
    } else {
        0
    }
}

/// A fixed-size rectangle centered in `area`, clamped to fit.
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

// ---- reminder helpers -------------------------------------------------------

/// Minutes from now until `days_ahead` days from today at `hour:min` local time.
fn minutes_at(days_ahead: i64, hour: u32, min: u32) -> Option<i64> {
    let now = Local::now();
    let date = (now + Duration::days(days_ahead)).date_naive();
    let naive = date.and_hms_opt(hour, min, 0)?;
    let target = Local.from_local_datetime(&naive).single()?;
    Some((target - now).num_minutes())
}

/// Minutes from now until a `YYYY-MM-DD` due date at 09:00 (None if past).
fn minutes_until_due(due: &str) -> Option<i64> {
    let date = NaiveDate::parse_from_str(due, "%Y-%m-%d").ok()?;
    let naive = date.and_hms_opt(9, 0, 0)?;
    let target = Local.from_local_datetime(&naive).single()?;
    let m = (target - Local::now()).num_minutes();
    (m > 0).then_some(m)
}

fn reminder_presets(task: &Task) -> Vec<(String, ReminderChoice)> {
    let mut v = vec![
        ("In 1 hour".into(), ReminderChoice::In(60)),
        ("In 3 hours".into(), ReminderChoice::In(180)),
    ];
    let evening = minutes_at(0, 18, 0)
        .filter(|m| *m > 0)
        .or_else(|| minutes_at(1, 18, 0));
    if let Some(m) = evening {
        v.push(("This evening (18:00)".into(), ReminderChoice::In(m)));
    }
    if let Some(m) = minutes_at(1, 9, 0) {
        v.push(("Tomorrow (09:00)".into(), ReminderChoice::In(m)));
    }
    v.push(("In 3 days".into(), ReminderChoice::In(3 * 1440)));
    v.push(("In 1 week".into(), ReminderChoice::In(7 * 1440)));
    v.push(("In 2 weeks".into(), ReminderChoice::In(14 * 1440)));
    if let Some(due) = &task.due {
        if let Some(m) = minutes_until_due(due) {
            v.push((format!("On due date {} (09:00)", due), ReminderChoice::In(m)));
        }
    }
    v.push(("Custom… (90m, 2d, 3w, 1h30)".into(), ReminderChoice::Custom));
    v
}

/// Parse a friendly duration into minutes: bare digits = minutes; otherwise a
/// sequence of `<n><unit>` with unit w/d/h/m (e.g. "2d", "3w", "1h30", "1d12h").
fn parse_duration_minutes(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return s.parse().ok().filter(|m| *m > 0);
    }
    let mut total: i64 = 0;
    let mut num = String::new();
    let mut saw_unit = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else if c == ' ' {
            continue;
        } else {
            let n: i64 = num.parse().ok()?;
            num.clear();
            let mult = match c.to_ascii_lowercase() {
                'w' => 10080,
                'd' => 1440,
                'h' => 60,
                'm' => 1,
                _ => return None,
            };
            total += n * mult;
            saw_unit = true;
        }
    }
    // Trailing number with no unit (e.g. the "30" in "1h30") counts as minutes.
    if !num.is_empty() {
        total += num.parse::<i64>().ok()?;
    }
    (saw_unit && total > 0).then_some(total)
}

fn human_minutes(m: i64) -> String {
    let (w, d, h, mi) = (m / 10080, (m % 10080) / 1440, (m % 1440) / 60, m % 60);
    let mut parts = Vec::new();
    if w > 0 {
        parts.push(format!("{}w", w));
    }
    if d > 0 {
        parts.push(format!("{}d", d));
    }
    if h > 0 {
        parts.push(format!("{}h", h));
    }
    if mi > 0 && w == 0 && d == 0 {
        parts.push(format!("{}m", mi));
    }
    if parts.is_empty() {
        "a moment".into()
    } else {
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.on_key(KeyCode::Char(c));
        }
    }

    #[test]
    fn add_status_category_grouping_and_fold() {
        let path = std::env::temp_dir().join(format!("mt-test-{}.json", std::process::id()));
        std::env::set_var("MONARCH_TODO_FILE", &path);
        let _ = std::fs::remove_file(&path);

        let mut app = App::new().unwrap();

        // Add a task, then give it a category.
        app.on_key(KeyCode::Char('a'));
        type_str(&mut app, "write tests");
        app.on_key(KeyCode::Enter);
        assert_eq!(app.store.tasks.len(), 1);
        // 'c' opens the category picker; with no categories yet, pick "New…".
        app.on_key(KeyCode::Char('c'));
        app.on_key(KeyCode::Down); // 0 = (no category), 1 = New category…
        app.on_key(KeyCode::Enter); // → free-text input
        type_str(&mut app, "work");
        app.on_key(KeyCode::Enter);
        assert_eq!(app.store.tasks[0].category.as_deref(), Some("work"));

        // Rows: a "work" header + the task under it.
        assert!(matches!(app.rows[0], Row::Header { .. }));
        assert!(matches!(app.rows[1], Row::Task(_)));

        // Select the header and fold it → the task row disappears.
        app.list.select(Some(0));
        app.on_key(KeyCode::Enter); // fold
        assert_eq!(app.rows.len(), 1);
        app.on_key(KeyCode::Enter); // unfold
        assert_eq!(app.rows.len(), 2);

        // A new task added while focused on the header inherits the category.
        app.list.select(Some(0));
        app.on_key(KeyCode::Char('a'));
        type_str(&mut app, "second");
        app.on_key(KeyCode::Enter);
        let second = app.store.tasks.iter().find(|t| t.text == "second").unwrap();
        assert_eq!(second.category.as_deref(), Some("work"));

        // Status quick action on a task.
        app.list.select(Some(1));
        app.on_key(KeyCode::Char('s'));
        assert!(app.store.tasks.iter().any(|t| t.status == Status::Doing));

        let reloaded = Store::load().unwrap();
        assert_eq!(reloaded.tasks.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn duration_parser() {
        assert_eq!(parse_duration_minutes("90"), Some(90));
        assert_eq!(parse_duration_minutes("90m"), Some(90));
        assert_eq!(parse_duration_minutes("2h"), Some(120));
        assert_eq!(parse_duration_minutes("1h30"), Some(90));
        assert_eq!(parse_duration_minutes("2d"), Some(2880));
        assert_eq!(parse_duration_minutes("3w"), Some(30240));
        assert_eq!(parse_duration_minutes("1d12h"), Some(2160));
        assert_eq!(parse_duration_minutes("abc"), None);
        assert_eq!(parse_duration_minutes(""), None);
    }
}
