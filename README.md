# monarch-todo

A fast, keyboard-driven TUI todo manager for Monarch. It is a second front-end
over the **same JSON store** the `monarch todo` bash CLI uses, so the shell
commands, the bar widget, and this TUI all read and write one source of truth.

## Features

- **Fast**: small native Rust binary, no async runtime; loads the store and
  renders instantly.
- **Quick actions**: act on the selected task with a single keypress (start,
  done, cancel, reopen, priority, category, delete).
- **Categories**: each task belongs to at most one category; tasks are shown
  grouped under foldable category headers.
- **States**: `todo` ○ · `doing` ◐ · `done` ✓ · `cancelled` ✗.
- **Priorities**: coloured dots 🔴 high · 🟠 medium · 🟡 low (stored as A/B/C).
- **Filtering**: incremental by title (`/`); fold categories to focus.
- **Reminders**: pick from presets (hours → weeks) or a free duration like
  `2d`, `3w`, `1h30`; schedules via `monarch-reminder`.
- **Shared store**: 100% compatible with `monarch todo` (CLI) — atomic writes,
  jq-identical formatting, tolerant of legacy `done` items.
- **Fits like fuzzel**: launched in a floating window sized by a Niri
  window-rule (`org.monarch.todo`); the layout is responsive.

## Build

```bash
cargo build --release   # → target/release/monarch-todo
cargo test              # logic tests (CRUD, filters, persistence)
```

## Usage

```bash
monarch-todo                 # no subcommand → TUI
                             # inside Monarch: `monarch launch todo` or Mod+T
```

### CLI (scripting)

A subcommand runs the CLI instead of the TUI — one Rust implementation of the
store, no shell/jq duplication:

```bash
monarch-todo add "Ship release" -p high -d 2026-06-20 -c Work
monarch-todo list [--all|--done|--status doing] [--category Work] [--format json]
monarch-todo done <id>          # id may be a unique prefix
monarch-todo status <id> doing
monarch-todo set <id> --text "…" --priority - --category Home
monarch-todo rm <id> | clear | edit
```

### Keys

| Key | Action |
|-----|--------|
| `↑`/`k` `↓`/`j`, `g`/`G` | navigate / top / bottom |
| `Space` / `Enter` | task: toggle done ↔ reopen · header: fold/unfold |
| `←` / `→` / `Z` | fold · unfold · fold-all (on headers) |
| `s` · `x` · `o` | start · cancel · reopen |
| `a` · `e` · `c` · `p` | add · edit text · category · priority |
| `m` | reminder — preset (hours→weeks) or custom `2d`/`3w` |
| `X` / `Delete` | delete (asks to confirm) |
| `/` · `Tab` | filter title · cycle scope (open/all/done) |
| `?` · `q`/`Esc` | help · quit (clears an active filter first) |

## Store

Resolved exactly like the bash CLI:

```
$MONARCH_TODO_FILE  ||  ${XDG_STATE_HOME:-$HOME/.local/state}/monarch/todo.json
```

Each task: `{ id, text, status, priority, created, due, category }`.
Legacy stores using `tags` are read transparently (the first tag becomes the
category).
