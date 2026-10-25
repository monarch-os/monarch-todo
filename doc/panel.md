# Native tasks panel

`monarch-todo panel`, the packaged Todo desktop entry, and Monarch's optional
Super+T integration open
`monarch/todo:panel`. The existing bar indicator opens the same panel. No terminal
or floating-window rule is involved. The optional `monarch-todo` package must be
version 0.2 or newer; the old launcher is not migrated while 0.1 is installed.
The native scrollbar/keyboard reveal requires Noctalia plugin API 33 or newer
(validated with Noctalia 5.3).

## Interaction

The panel is 580 × 320 px with a fixed-height native scroll viewport for three
complete task rows. Its scrollbar represents the full list and supports the
wheel and dragging. Keyboard navigation and newly created task focus use native
scroll-into-view. Rendering is virtualized with an overscan row below and
transparent spacers, rather than building a UI node for every task.
Mouse scrolling updates the range and keeps selection within the visible rows;
periodic refresh does not pull the viewport back to an old selection.
List refreshes are single-flight: a slow backend cannot accumulate polling
processes. Mutations invalidate stale responses; failed process launches release
the busy state and preserve the draft. Form reset, preview scheduling and saving
share helpers so keyboard entry points follow the same state transitions.
The title prominently shows the matching task count (for example `Tasks (4)`).
When more than three tasks match, the header also shows the visible range and total
(for example `2–4 / 4`). Creation confirms success and focuses the added task,
even when its sort position lies outside the initial slice; that focus survives
closing/reopening the panel. Selection is remembered across normal opens too.
The header contains only the title and a passive view label, with no action
buttons. Every header action remains available through the shortcuts below;
the footer groups them into readable keycaps and separate, higher-contrast action
labels, including snooze and undo. The help is contextual in creation, reminder
and delete-confirmation modes. Escape, Super+T or an outside
click closes the panel without changing a task's status or stopwatch.
Each task has one optional `#Category` badge. It is centered vertically across
the whole two-line task row, rather than aligned only with the title. Categories
use a stable name-based choice from Monarch's six categorical accent colors,
with dark/light variants; the palette is reused for further names. The same name
keeps the same color regardless of task order or scope. Metadata remains
separate and readable. Long labels are width-limited to leave room for
the title; uncategorized tasks have no empty badge. Preview badges use the same
colors. Existing single categories are preserved automatically.

The first field searches existing tasks. Ctrl+N switches to creation;
typing `date > title` switches directly to creation. The backend previews the
parsed date, recurrence and priority after a short debounce.

```text
+5m > Check the oven !
tomorrow at 9am > Call the garage !high
demain à 9h30 > Traitement @daily !critical #(Vie privée)
next friday 3pm > Report @weekly #Work
Préparer le devis #Travail
```

Dates without an hour notify at the end of the local day. Recurrence follows a
calendar cadence and advances on completion, skipping missed occurrences. A
delivered reminder does not complete a task. The backend README documents the
full grammar, DST behavior and storage guarantees. Standalone `monarch reminder`
remains separate.

| Key | Action |
| --- | --- |
| Up/Down, Ctrl+K/J | Select task |
| Ctrl+N | Create |
| Enter, Ctrl+E | Edit selection |
| Enter in creation/edit mode | Save and close |
| Ctrl+Enter | Save and continue; create directly from search |
| Ctrl+Space | Complete/reopen |
| Ctrl+S | Start/pause the task and its stopwatch |
| Ctrl+M | Snooze a reminder: +5 min, +1 h, tomorrow at 9am, or a custom date |
| Ctrl+Alt+Z | Undo the last user action (one level) |
| Ctrl+Shift+X | Cancel task |
| Ctrl+D, then Enter | Delete with confirmation |
| Tab | Open / reminders / finished |
| Ctrl+R | Reload |
| Alt+Left | Return to the list without saving |
| Escape | Close |

Editing reuses the same input syntax. Remove the date prefix and any recurrence
suffix to remove its reminder; remove only the recurrence suffix to stop recurrence. Remove a priority/category
suffix to clear that field. Only one category suffix is accepted; use
`#(Category with spaces)` for multiword names. A backslash
keeps suffix-looking title words literal. `> Literal > title` is unscheduled.

The panel invokes only the CLI, never todo.json. The backend owns locking,
natural-language parsing, recurrence and systemd timers. An error keeps the
input; a timer failure after saving a new task clears the input and reports the
failure instead of offering to create a duplicate. Task data is refreshed on
every open, after mutations and every five seconds while open. Backend process
failures and invalid responses remain visible.

An active task shows a live HH:MM:SS stopwatch in its existing detail line, so
tracking does not increase panel height. Only visible running timers are redrawn
once per second, without recreating the input or polling a process each second.
Pausing preserves the accumulated duration; resuming adds a new segment.
Completion/cancellation stops tracking, while reopening retains the total. A
recurring successor starts at zero. The backend persists timing independently of
reminders and of whether Noctalia is running. Until paused, wall time includes
suspend and time spent outside the session; multiple tasks may run independently.

## Quick snooze and undo

Ctrl+M opens a compact reminder view for the selection. Pick
+5 min, +1 h or tomorrow at 9am, or type a natural date and press Enter. The
preview uses the backend parser. Snoozing a notification preserves the original
due date and recurring cadence, unlike explicitly changing the series schedule.
It does not interrupt the task's stopwatch. The notification itself offers a
direct +5-minute action through Monarch's notification helper.

Ctrl+Alt+Z restores the previous user action, even after
closing/reopening the panel. Undo does not discard an active creation/edit/reminder
draft, and Ctrl+Z is left available for normal text editing. Failed/no-op actions
and automatic reminders do not replace history. The backend refuses conflicting
external changes or deliveries instead of restoring stale task data. Undoing a
recurring completion removes its successor only while that successor is unchanged.

## Installation and upgrade

The experimental multi-category format is read using its first category. Before
rewriting such data, the backend privately backs up the original task file and
undo history as `*.before-single-category.json` (numbered copies if different
snapshots already exist). No secondary labels are discarded without a backup.

The `monarch-todo` package owns backend, plugin, translations, desktop entry and
systemd user service. `make DESTDIR=... install` installs this complete payload.
Noctalia 5.3 discovers its local data directory, not system XDG_DATA_DIRS, so
`monarch-todo setup-desktop` registers a symlink under the user's Noctalia plugins
directory pointing to `/usr/share/monarch-todo/noctalia/monarch-todo`. There is no
user copy to reseed on upgrades. `panel` performs setup before opening; package
manager hooks never modify user homes or enable services as root.

Setup backs up previous plugin directories and recognized legacy user desktop/
service files under `$XDG_STATE_HOME/monarch-todo/desktop-backups`, preserving
custom content before replacing it. Unrelated plugin directories are rejected;
custom user services remain overrides. The system replacement must exist before
legacy files are moved. Reminders use `/usr/lib/systemd/user/monarch-todo-reminders.service`.
Without a running shell, setup registers files and the service; first `panel`
activation enables the plugin. `remove-desktop` disables the user's integration
and removes only the owned link, preserving tasks and backups.

Monarch only owns installation/removal commands, its bar entry and Super+T. Its
user reconciliation delegates to `setup-desktop` before migrating the Niri
shortcut. The existing task JSON does not move. The published recipe still
targets 0.1.2 until v0.2.0 exists; its packaging function supports the new Makefile
without referring to an unpublished tag. Future panel/backend changes need only
one `monarch-todo` package release.

## Validation

```sh
cargo build
python3 test/desktop.py
python3 test/panel.py /path/to/luau
noctalia plugins lint noctalia/monarch-todo
```

For an isolated development session, `panel-open monarch/todo:panel` accepts a
JSON context with absolute `backend` (backend binary or test wrapper). This is a
trusted local IPC input, never saved. Production uses the packaged binary. The test
wrapper must set `MONARCH_TODO_FILE` to a temporary store; never use live data
for UI tests. `strings` emits the translations embedded in the backend, following
Monarch's language preference without depending on Monarch's files or Python.
