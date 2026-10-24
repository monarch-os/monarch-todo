# monarch-todo

Task and reminder backend for Monarch's keyboard-first Noctalia panel. Version
0.2 replaces the terminal UI with the native panel; the CLI and existing JSON
store remain available. Running the binary without arguments prints help.

## Desktop package

The same package ships the backend, its Noctalia plugin, translated panel text,
application entry and user reminder service. See [panel documentation](doc/panel.md).

```sh
monarch-todo panel           # register and open the native Noctalia panel
monarch-todo setup-desktop   # register the plugin and enable reminders as this user
monarch-todo remove-desktop  # disable integration before removal; keeps task data
```

Noctalia 5.3+ and Python (for setup only) are needed for the native panel. The
backend's normal CLI remains usable without Noctalia or Monarch. Plugin sources
live in `/usr/share/monarch-todo`, exposed by a user symlink, not copied. Pacman
therefore upgrades the backend and interface together. Monarch may supply the
Super+T shortcut, bar widget and convenient install/remove commands separately.
Translations are embedded from `i18n/en.json` and `i18n/fr.json` and follow
`MONARCH_LANGUAGE`, the Monarch language preference, then locale. For development,
`MONARCH_TODO_DATA_DIR` selects an absolute package data directory.

## Quick input

```sh
monarch-todo parse 'tomorrow at 9am > Call the garage !high'
monarch-todo quick '+5m > Check the oven !'
monarch-todo quick 'demain à 9h30 > Traitement @daily !critical #(Vie privée)'
monarch-todo quick 'next friday 3pm > Send the report @weekly #Work'
monarch-todo quick 'tomorrow > Water plants @every:2d !low'
```

`parse` returns a JSON preview without changing files or scheduling anything.
`quick` creates a task and returns its JSON. `update <id> '<input>'` replaces
its editable fields using the same grammar. No shell code in input is executed.

Only text **before the first `>`** is parsed as a date. Without a separator,
ordinary words such as `today` remain in the title. An empty date prefix
(`> Literal > title`) explicitly creates an unscheduled task. A backslash before
a suffix-looking word keeps it literally in the title: `Task \!high`.

### Dates

Natural English dates are parsed by [interim](https://crates.io/crates/interim):
`today`, `tomorrow`, `next friday`, `January 1`, `2027-01-01`, `in 2 hours`,
`+5m`, `+1h`, `in 5 days at 15:00`. Numeric dates use the UK day/month dialect.

A normalization layer supports common French day/month/unit names and phrases:
`aujourd'hui`, `demain`, `après-demain`, `dans 2 heures`, `demain à 9h30`.
This is not a general-purpose French natural-language parser. `tomorow` is
accepted as an alias for `tomorrow`.

Dates without a clock time schedule a notification at **23:59:59 local time**.
Relative durations preserve the current time plus the duration. Invalid/past
natural dates fail without saving. Exact RFC3339 timestamps are also accepted,
including past timestamps when round-tripping existing task schedules.

### Suffixes

- `!low`, `!normal`, `!high`, `!critical`; bare `!` means critical.
  Legacy A/B/C priorities remain readable. Critical notifications use critical
  urgency; high and normal use normal urgency; low uses low urgency.
- `@daily`, `@weekly`, `@monthly`, `@yearly`, or `@every:2d`.
  Interval units: `m`, `h`, `d`, `w`, `mo`, `y`. A recurrence requires an explicit
  first date before `>`. `@quotidien`, `@hebdomadaire`, `@mensuel`, `@annuel`
  are accepted too.
- One optional category suffix: `#Work` or `#(Category with spaces)`.
  It can be mixed with priority/recurrence in any order. Multiple category
  suffixes are rejected rather than silently replacing a name. Escape literal
  parentheses/backslashes inside a category: `#(Client \(France\))`.

### Recurrence semantics

A recurring occurrence remains open until completed. Delivering its reminder
does **not** complete it or repeatedly notify. Completing it keeps its history
and creates exactly one successor on the original cadence. Reopening/completing
an old occurrence cannot create duplicate successors. Missed dates are skipped
to the next future occurrence; no backlog of duplicate tasks is generated.

Days/weeks/months/years follow the local calendar (DST-aware); minutes/hours are
elapsed durations. Missing local times during a DST transition are skipped.
Month ends are clamped while retaining the original day anchor (31 January →
28 February → 31 March). Ambiguous recurrence times choose the earlier instant.

## CLI

```sh
monarch-todo list --all --format json
monarch-todo add 'Task' --priority high --due 2027-01-01 --category Work
monarch-todo set <id> --category Client
monarch-todo list --category Client --format json
monarch-todo set <id> --text 'New title' --priority critical
monarch-todo reminder <id> '+1h'
monarch-todo reminder <id> -       # also removes recurrence
monarch-todo snooze <id> '+5m'     # preserves the original recurring cadence
monarch-todo undo                 # last user action, one level
monarch-todo done <id>
monarch-todo status <id> doing     # todo, doing, done, cancelled
monarch-todo rm <id>
monarch-todo clear                # remove finished/cancelled history
monarch-todo reload-reminders
```

Reminders use transient systemd user timers, independently of the panel or
Noctalia process. Monarch installs a login oneshot to re-arm future reminders
and notify about missed reminders. Every CLI mutation reconciles timers;
completed/cancelled/deleted tasks no longer notify. Stale timer invocations are
checked against the expected scheduled date.

Timers request one-second scheduling accuracy rather than systemd's default
minute-wide coalescing window. Their services use `ExitType=cgroup` so the
notification action listener survives the initial delivery command and is
collected after the notification is acted on or dismissed.

Scheduling failure returns a nonzero exit code **after saving the task**. `quick`
still outputs the saved task JSON, so clients must not blindly retry creation.
Run `reload-reminders` after correcting the scheduling problem. Failed delivery
keeps the reminder pending for retry; a process crash between notification and
saving can deliver it again (delivery is not exactly-once).

### Snooze and undo

`snooze <id> '<date>'` schedules a new notification for an open task, including
after its previous reminder fired. It preserves the task's due date, tracking
and recurrence cadence. Repeated snoozes retain the original occurrence anchor;
completing the task still advances on that cadence. Explicit `reminder` or a
changed date/rule in `update` instead reschedules the series intentionally.

Notifications sent through Monarch's helper offer **Remind in 5 minutes**. Its
action invokes this backend with separate arguments (no user text is executed),
and inherits the reminder's store environment. The plain `notify-send` fallback
has no interactive action. Other delays are available in the panel or CLI.

Undo persists one user action in a private `.undo.json` sidecar next to the store.
It covers creation, editing, reminder/snooze changes, status changes, deletion
and clearing finished tasks; it has no redo. A no-op or validation error does not
replace history. Automatic reminder delivery/reconciliation and raw `$EDITOR`
edits are not journalled. The sidecar contains before/after task data, including
deleted tasks, until replaced or consumed; include it in private backups.

Undo validates every affected task against its saved after-image before changing
anything. Later delivery or external changes to those tasks cause a conflict,
not an overwrite; changes to unrelated tasks are preserved. Undoing recurring
completion removes the untouched successor and restores its original occurrence.
Undoing pause/completion/deletion of a running task resumes its clock without
counting the intervening stopped/deleted period. Undoing an accidental start
discards that new segment. Reminder timers are reconciled after restoration,
including delivery of restored overdue reminders.

The journal is written before the task store, under the same lock. A crash before
the store write makes after-image validation refuse the unapplied undo. After a
successful undo the record is consumed; scheduler failure does not roll back the
restored tasks (use `reload-reminders` to retry timer reconciliation).

## Time tracking

`status <id> doing` starts/resumes a stopwatch; `status <id> todo` pauses it.
Completing or cancelling stops it. Reopening retains the accumulated duration,
but does not resume automatically. Repeating the same start/stop is idempotent.
Editing text, priority, category or reminder does not reset tracked time.

`time_spent_ms` stores completed segments; `started_at_ms` stores the current
segment's Unix timestamp in milliseconds, or null while stopped. JSON lists add
the computed `elapsed_ms`; plain lists display HH:MM:SS. This computed value is
not written back into the store. A recurring successor starts at zero and its
previous occurrence keeps its own duration.

No background process or systemd timer is needed for tracking. Time continues
until paused, including while the panel is closed, during suspend and across
reboots. This measures elapsed wall time, not keyboard activity; clock rollback
cannot subtract accumulated time. Multiple tasks may run independently. Legacy
doing tasks without a start timestamp have no invented historical duration.

## Persistence and compatibility

Each task stores one optional `category`. `list --category` filters it, and
`set --category -` clears it. Repeated `--category` options and multiple quick
input suffixes are rejected. Recurring successors retain that single category.

The experimental `categories` array and older `tags` are read using the first
nonempty category (an explicitly empty `categories` array remains cleared).
Before any multi-category data is rewritten, the original task file and undo
history are backed up privately as `*.before-single-category.json`. Different
snapshots receive numbered backups rather than overwriting earlier ones; a
backup failure stops the operation before modifying tasks. Reads do not rewrite
the main task file; the single-category format is saved on the next mutation.
Undo snapshots use the same compatibility reader and are backed up before any
multi-category snapshot is consumed. The backup files retain all former labels.

Path: `$MONARCH_TODO_FILE`, otherwise
`${XDG_STATE_HOME:-$HOME/.local/state}/monarch/todo.json`.

The existing array format is preserved, including legacy `done` and `tags`
imports. New recurrence/history/delivery fields have defaults. A stable sidecar
lock protects the whole read-modify-write transaction; writes use fsync and
atomic rename. Keep user data backed up. Downgrading to 0.1 can discard fields
it does not understand and is not supported after using recurrence/criticality.

## Tests

The required entry point is `LUAU_BIN=/path/to/luau bash test/run`. It runs every
suite below, formatting, Clippy and the release build used for package layout
tests. Dependencies: Rust (rustfmt/Clippy), Python 3.11+, Bash, make, coreutils and
Luau. CI installs pinned Luau 0.744 with a verified archive checksum. The test
inventory is `test/integration.py` (CLI), `test/desktop.py` (package activation),
`test/panel.py` (actual Luau with a mock host), and Rust unit tests under `src/`.

```sh
cargo test
cargo build
python3 test/integration.py
python3 test/desktop.py
python3 test/panel.py /path/to/luau
cargo clippy --all-targets -- -D warnings
```

Integration tests isolate the store and mock systemd/notifications, covering
concurrent writes, migration, recurrence, stale timers and delivery failures.

Build packaging with `cargo build --release` then `make DESTDIR=/staging install`.
This stages only system files; desktop setup must run as the user, not in a
root package hook. The official recipe should advance to v0.2.0 only after its
release tag is published.
