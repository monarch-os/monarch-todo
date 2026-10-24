#!/usr/bin/env python3
"""Run the actual panel under Luau with a deterministic, asynchronous host mock.

Usage: python3 test/panel.py /path/to/luau
"""
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[1]
mock = r'''
local calls, rendered, now = {}, nil, 1000
local darkMode = true
local rejectProcess = false
local values = {
  strings = { ["todo.keyEscape"] = "Esc", ["todo.keySpace"] = "Space", ["todo.keyShift"] = "Shift" },
  tasks = {
    { id = "abc", text = "First", status = "todo", priority = "Critical", category = "Monarch" },
    { id = "def", text = "Other", status = "done" },
  },
  parsed = { text = "New", priority = "A", reminder = "2026-10-24T09:00:00+02:00", category = "Maison" },
  saved = { id = "new-id", text = "New" },
}
noctalia = {
  isDarkMode = function() return darkMode end,
  json = { decode = function(value) return values[value] end },
  runAsync = function(cmd, callback)
    if rejectProcess then return false end
    table.insert(calls, { cmd = cmd, callback = callback }); return true
  end,
  nowMs = function() return now end,
  fuzzyScore = function(q, text) return text:lower():find(q:lower(), 1, true) and 1 or nil end,
}
local closed = 0
panel = {
  render = function(tree) rendered = tree end,
  close = function() closed += 1; onClose() end,
  setNeedsFrameTick = function(_) end,
}
ui = setmetatable({}, { __index = function(_, name)
  return function(props, children) return { type = name, props = props, children = children or {} } end
end })
local function reply(index, stdout, code, stderr)
  calls[index].callback({ exitCode = code or 0, stdout = stdout or "", stderr = stderr or "" })
end
local function contains(node, text)
  if node.props.text and node.props.text:find(text, 1, true) then return true end
  for _, child in node.children do if contains(child, text) then return true end end
  return false
end
local function input(node)
  if node.type == "input" then return node.props end
  for _, child in node.children do local found = input(child); if found then return found end end
end
local function keyed(node, key)
  if node.props.key == key then return node end
  for _, child in node.children do local found = keyed(child, key); if found then return found end end
end
'''
tests = r'''
onOpen("")
assert(#calls == 2)
reply(1, "strings")
reply(2, "tasks")
assert(contains(rendered, "First"))
assert(not contains(rendered, "Other"))
local badge = keyed(rendered, "category:Monarch")
assert(badge and badge.type == "row" and contains(badge, "#Monarch"), "category must have its own visible badge")
assert(badge.props.fill:match("^#%x%x%x%x%x%x33$") and badge.props.borderWidth == 1)
assert(badge.children[1].props.color == "on_surface", "category must not use faint metadata text")
assert(keyed(rendered, "abc").props.align == "center", "badges must be centered across the whole task row")
local header = keyed(rendered, "header")
assert(#header.children == 2, "header must contain only the title and passive view label")
for _, child in header.children do
  assert(child.type == "label" and child.props.onClick == nil, "header must have no action buttons")
end
assert(keyed(rendered, "view").props.text == "open")
local footer = keyed(rendered, "hint")
assert(footer.type == "column" and #footer.children == 4, "keyboard help must have a divider and three grouped rows")
local firstShortcut = footer.children[2].children[1]
assert(firstShortcut.children[1].props.key == "keycap")
assert(firstShortcut.children[1].props.fill == "surface_variant")
assert(firstShortcut.children[1].children[1].props.text == "↑↓")
assert(firstShortcut.children[1].children[1].props.fontFamily == "monospace")
assert(firstShortcut.children[2].props.text == "help.choose", "bindings and definitions must be separate labels")
assert(firstShortcut.children[2].props.color == "on_surface/0.9" and firstShortcut.children[2].props.fontSize == 9)
assert(firstShortcut.children[1].children[1].props.fontSize == 9)
assert(firstShortcut.children[1].props.paddingV == 0, "keycaps must keep the footer compact")
assert(contains(footer, "Ctrl+M") and contains(footer, "Ctrl+Alt+Z") and contains(footer, "Esc"))
onKey("tab", true)
assert(keyed(rendered, "view").props.text == "reminders", "Tab must still switch views without a button")
onKey("tab", true)
assert(keyed(rendered, "view").props.text == "done")
onKey("tab", true)
assert(keyed(rendered, "view").props.text == "open")
onKey("ctrl+space", true)
assert(calls[#calls].cmd:find("'status' 'abc' 'done'", 1, true))
local pending = #calls
onKey("ctrl+space", true)
assert(#calls == pending, "busy mutations must not be duplicated")
reply(pending, "")
reply(#calls, "tasks")
onNew()
assert(#keyed(rendered, "hint").children == 2, "creation help must use a compact contextual row")
assert(contains(keyed(rendered, "hint"), "Alt+←") and contains(keyed(rendered, "hint"), "help.back"))
local beforeTyping = #calls
now += 200
onFrameTick()
assert(#calls == beforeTyping, "opening an empty creation form must not immediately produce a parse error")
onQueryChanged("+5m > New !high")
local stableKey = input(rendered).key
now += 200
onFrameTick()
local parseCall = #calls
assert(calls[parseCall].cmd:find("'parse' '+5m > New !high'", 1, true))
onQueryChanged("+1h > New !high")
reply(parseCall, "parsed")
assert(not contains(rendered, "2026-10-24"), "stale parse result must be ignored")
assert(input(rendered).key == stableKey, "typing must not recreate the input")
now += 200
onFrameTick()
reply(#calls, "parsed")
assert(contains(rendered, "2026-10-24"))
assert(contains(keyed(rendered, "category:Maison"), "#Maison"), "creation preview must show the category badge too")
onKey("ctrl+return", true)
assert(calls[#calls].cmd:find("'quick' '+1h > New !high'", 1, true))
reply(#calls, "saved")
assert(closed == 0, "save-and-continue must keep the panel open")
reply(#calls, "tasks")
onNew()
onQueryChanged("bad-date > Keep my input")
onSubmit()
reply(#calls, "", 1, "Invalid date")
assert(input(rendered).value == "bad-date > Keep my input")
assert(contains(rendered, "Invalid date"))
reply(#calls, "tasks")
onClose()
local old = #calls
onOpen("")
reply(old, "tasks")
assert(not contains(rendered, "First"), "closed-session callbacks must be ignored")
reply(#calls - 1, "strings")
reply(#calls, "tasks")
onKey("ctrl+d", true)
assert(contains(rendered, "deleteConfirm"))
assert(contains(keyed(rendered, "hint"), "help.delete"), "delete confirmation must not advertise Enter as Edit")
onSubmit()
assert(calls[#calls].cmd:find("'rm' 'abc'", 1, true))
reply(#calls, "")
reply(#calls, "tasks")
onNew()
onQueryChanged("+1h > Saved but unscheduled")
onSubmit()
reply(#calls, "saved", 1, "Task saved, timer failed")
assert(input(rendered).value == "", "saved task must not be created twice after timer failure")
assert(contains(rendered, "Task saved, timer failed"))
reply(#calls, "tasks")
values.tasks = {
  { id = "1", text = "First", status = "todo" },
  { id = "2", text = "Second", status = "todo" },
  { id = "3", text = "Third", status = "todo" },
  { id = "4", text = "Fourth", status = "todo" },
}
onKey("ctrl+r", true)
reply(#calls, "tasks")
assert(contains(rendered, "First") and contains(rendered, "Third"))
assert(keyed(rendered, "header").children[1].props.text == "title (4)", "total count must be prominent before navigating the list")
local scroll = keyed(rendered, "task-list")
assert(keyed(rendered, "list-viewport").props.height == 134 and scroll.props.flexGrow == 1,
  "the wrapper must provide an exact compact height to the native scroll view")
assert(type(scroll.props.onScroll) == "function")
assert(keyed(rendered, "4").props.height == 38, "overscan rows must use fixed geometry for an accurate scrollbar")
onListScrolled("40", "40")
assert(keyed(rendered, "view").props.text == "open · 2–4 / 4", "mouse scrolling must update the range")
assert(keyed(rendered, "task-list").props.revealKey == "", "mouse scrolling must not be pulled back to an old selection")
assert(#keyed(rendered, "categories").children == 0, "uncategorized tasks must not have empty badges")
onKey("down", true)
onKey("down", true)
onKey("down", true)
assert(contains(rendered, "Fourth"), "keyboard must reach tasks outside the compact viewport")
assert(keyed(rendered, "task-list").props.revealKey == "4", "keyboard navigation must reveal the selected row natively")
onListScrolled("40", "40")
onKey("ctrl+r", true)
reply(#calls, "tasks")
assert(keyed(rendered, "view").props.text == "open · 2–4 / 4", "background refresh must preserve mouse scrolling")
assert(keyed(rendered, "task-list").props.revealKey == "")
values.tasks = {
  { id = "timed", text = "Tracking", status = "doing", time_spent_ms = 61000, started_at_ms = now - 2000 },
}
onKey("ctrl+r", true)
reply(#calls, "tasks")
assert(contains(rendered, "00:01:03"), "running time must include previous segments")
local trackingKey = input(rendered).key
now += 2000
onFrameTick()
assert(contains(rendered, "00:01:05"), "visible timers must update without a backend request")
assert(input(rendered).key == trackingKey, "timer ticks must preserve the input control")
onKey("ctrl+s", true)
assert(calls[#calls].cmd:find("'status' 'timed' 'todo'", 1, true))
values.tasks[1].status = "todo"
values.tasks[1].time_spent_ms = 65000
values.tasks[1].started_at_ms = nil
reply(#calls, "")
reply(#calls, "tasks")
now += 2000
onFrameTick()
assert(contains(rendered, "00:01:05"), "paused timers must remain frozen")
onKey("ctrl+s", true)
assert(calls[#calls].cmd:find("'status' 'timed' 'doing'", 1, true))
values.tasks[1].status = "doing"
values.tasks[1].started_at_ms = now
reply(#calls, "")
reply(#calls, "tasks")
onKey("ctrl+m", true)
assert(contains(rendered, "snoozing"))
assert(input(rendered).placeholder == "snoozePlaceholder")
onSnoozePreset("+1h")
assert(calls[#calls].cmd:find("'snooze' 'timed' '+1h'", 1, true))
reply(#calls, "")
reply(#calls, "tasks")
assert(closed == 0)
onKey("ctrl+alt+z", true)
assert(calls[#calls].cmd:find("'undo'", 1, true))
reply(#calls, "")
reply(#calls, "tasks")
onSnooze()
onQueryChanged("+5m")
now += 200
onFrameTick()
assert(calls[#calls].cmd:find("'parse' '+5m > Reminder'", 1, true))
reply(#calls, "parsed")
onSubmit()
assert(calls[#calls].cmd:find("'snooze' 'timed' '+5m'", 1, true))
reply(#calls, "", 1, "Cannot schedule reminder")
reply(#calls, "tasks")
assert(input(rendered).value == "+5m", "snooze errors must preserve the input")
onKey("alt+left", true)
assert(input(rendered).value == "")
onNew()
onQueryChanged("Do not lose this draft")
local count = #calls
onKey("ctrl+alt+z", true)
assert(#calls == count, "undo must not discard an unsaved draft")
assert(input(rendered).value == "Do not lose this draft")
local beforeClose = #calls
local started = values.tasks[1].started_at_ms
onClose()
assert(#calls == beforeClose, "closing the panel must never pause or modify a task")
now += 4000
onOpen("")
assert(#calls == beforeClose + 2, "reopening must only load strings and tasks")
reply(#calls - 1, "strings")
reply(#calls, "tasks")
assert(contains(rendered, "doing"), "running status must survive close/reopen")
assert(values.tasks[1].started_at_ms == started, "close/reopen must preserve the running segment")
values.tasks[1].category = "Une catégorie personnelle particulièrement longue"
onKey("ctrl+r", true)
reply(#calls, "tasks")
badge = keyed(rendered, "category:" .. values.tasks[1].category)
assert(badge.children[1].props.maxWidth == 140 and badge.children[1].props.maxLines == 1,
  "long categories must be bounded to keep space for the title")
assert(contains(badge, "#" .. values.tasks[1].category), "category text must not be changed or lost")
values.tasks[1].categories = { "Work", "Home", "Client" }
values.tasks[1].category = "Work"
onKey("ctrl+r", true)
reply(#calls, "tasks")
local workColor = keyed(rendered, "category:Work").props.fill
assert(keyed(rendered, "category:Home") == nil and keyed(rendered, "category:Client") == nil,
  "only the single primary category must be shown, even for an older backend response")
assert(keyed(rendered, "category-overflow") == nil)
assert(#keyed(rendered, "categories").children == 1)
local timed = values.tasks[1]
table.insert(values.tasks, { id = "home", text = "Home task", status = "todo", category = "Home" })
onKey("ctrl+r", true)
reply(#calls, "tasks")
local homeColor = keyed(rendered, "category:Home").props.fill
assert(workColor ~= homeColor, "different categories must have different categorical accents")
assert(keyed(rendered, "category:Work").children[1].props.maxWidth == 140)
values.tasks = { values.tasks[2], values.tasks[1] }
onKey("ctrl+r", true)
reply(#calls, "tasks")
assert(keyed(rendered, "category:Work").props.fill == workColor, "colors must not depend on task order")
darkMode = false
render()
assert(keyed(rendered, "category:Work").props.fill ~= workColor, "accents must adapt to dark/light mode")
darkMode = true
render()
assert(keyed(rendered, "category:Work").props.fill == workColor)
timed.category = "Client (France) C:\\Users"
render()
onEdit()
assert(input(rendered).value:find("#(Client \\(France\\) C:\\\\Users)", 1, true), "editing must preserve the single category and escape its parentheses/backslashes")
assert(not input(rendered).value:find("#(Home)", 1, true), "editing must never reintroduce secondary categories")
onKey("alt+left", true)
values.tasks = {
  { id = "top1", text = "Top first", status = "todo" },
  { id = "top2", text = "Top second", status = "todo" },
  { id = "top3", text = "Top third", status = "todo" },
}
onKey("ctrl+r", true)
reply(#calls, "tasks")
onNew()
onQueryChanged("New fourth")
onKey("ctrl+return", true)
table.insert(values.tasks, { id = "new-id", text = "New fourth", status = "todo" })
reply(#calls, "saved")
reply(#calls, "tasks")
assert(keyed(rendered, "new-id").props.fill == "surface_variant", "a new task beyond the first three must be visible and selected")
assert(keyed(rendered, "task-list").props.revealKey == "new-id")
assert(keyed(rendered, "view").props.text == "open · 2–4 / 4", "the viewport must expose its range and total")
assert(contains(rendered, "created"), "successful creation must be confirmed")

values.saved = { id = "fifth", text = "New fifth" }
onNew()
onQueryChanged("New fifth")
onSubmit()
table.insert(values.tasks, { id = "fifth", text = "New fifth", status = "todo" })
local beforeSaveClose = closed
reply(#calls, "saved")
assert(closed == beforeSaveClose + 1)
onOpen("")
reply(#calls - 1, "strings")
reply(#calls, "tasks")
assert(keyed(rendered, "fifth").props.fill == "surface_variant", "Enter-created task must remain selected after reopening")
assert(keyed(rendered, "view").props.text == "open · 3–5 / 5")
assert(contains(rendered, "created"))
onKey("tab", true)
onKey("tab", true)
assert(keyed(rendered, "view").props.text == "done")
values.saved = { id = "sixth", text = "New from finished view" }
onNew()
onQueryChanged("New from finished view")
onKey("ctrl+return", true)
table.insert(values.tasks, { id = "sixth", text = "New from finished view", status = "todo" })
reply(#calls, "saved")
reply(#calls, "tasks")
assert(keyed(rendered, "sixth").props.fill == "surface_variant", "creation from a filtered scope must return to open tasks")
assert(keyed(rendered, "view").props.text == "open · 4–6 / 6")
values.saved = { id = "seventh", text = "Saved before timer failure" }
onNew()
onQueryChanged("+1h > Saved before timer failure")
onSubmit()
table.insert(values.tasks, { id = "seventh", text = "Saved before timer failure", status = "todo" })
reply(#calls, "saved", 1, "Timer failed after saving")
reply(#calls, "tasks")
assert(keyed(rendered, "seventh").props.fill == "surface_variant")
assert(contains(rendered, "Timer failed after saving"))
assert(input(rendered).value == "")
onKey("up", true)
onClose()
onOpen("")
reply(#calls - 1, "strings")
reply(#calls, "tasks")
assert(keyed(rendered, "sixth").props.fill == "surface_variant", "user selection must survive outside close/reopen")
onKey("ctrl+s", true)
local moved
for index, task in values.tasks do
  if task.id == "sixth" then moved = table.remove(values.tasks, index); break end
end
moved.status = "doing"
table.insert(values.tasks, 3, moved)
reply(#calls, "")
reply(#calls, "tasks")
assert(keyed(rendered, "sixth").props.fill == "surface_variant", "starting/reordering a task must not jump selection to another task")
values.tasks = {}
for index = 1, 1000 do
  table.insert(values.tasks, { id = "big" .. index, text = "Big " .. index, status = "todo" })
end
onKey("ctrl+r", true)
reply(#calls, "tasks")
local function assertVirtualExtent()
  local node = keyed(rendered, "task-list")
  local height, count = 0, 0
  for _, child in node.children do
    height += child.props.height or 0
    if child.type == "row" then count += 1 end
  end
  height += math.max(0, #node.children - 1) * node.props.gap
  assert(count <= 7, "large lists must not build a UI node for every task")
  assert(height == 39998, "virtual spacers must represent the entire list, not only the visible slice")
end
assertVirtualExtent()
onListScrolled("39872", "79744")
assert(keyed(rendered, "view").props.text == "open · 499–502 / 1000", "scroll metrics must remain correct at 2x scale")
assertVirtualExtent()
onListScrolled("79744", "79744")
assert(keyed(rendered, "view").props.text == "open · 997–1000 / 1000")
assertVirtualExtent()
onKey("down", true)
onKey("down", true)
assert(keyed(rendered, "task-list").props.revealKey == "big1000")
local staleScroll = keyed(rendered, "task-list").props.onScroll
onQueryChanged("Big 999")
staleScroll("79744", "79744")
assert(keyed(rendered, "task-list").props.revealKey == "big999", "filtering must reveal the first matching task")
assert(keyed(rendered, "header").children[1].props.text == "title (1)")
onKey("ctrl+r", true)
local pendingRefresh = #calls
onKey("ctrl+r", true)
now += 6000
onFrameTick()
assert(#calls == pendingRefresh, "slow refreshes must not spawn overlapping list processes")
reply(pendingRefresh, "tasks")
rejectProcess = true
onKey("ctrl+r", true)
assert(contains(rendered, "backendFailed"), "failed process launch must report an error")
onNew()
onQueryChanged("Preserve draft")
onSubmit()
assert(input(rendered).value == "Preserve draft" and input(rendered).enabled,
  "failed mutation launch must preserve input and release busy state")
rejectProcess = false
onSubmit()
assert(calls[#calls].cmd:find("'quick' 'Preserve draft'", 1, true), "failed launch must remain retryable")
print("ok - async results, stable inputs, errors, keyboard, delete confirmation and scheduling failures")
'''
with tempfile.TemporaryDirectory(prefix="todo-panel-", dir="/tmp/opencode") as directory:
    path = Path(directory) / "test.luau"
    path.write_text(mock + "\n" + (root / "noctalia/monarch-todo/panel.luau").read_text() + "\n" + tests)
    subprocess.run([sys.argv[1] if len(sys.argv) > 1 else "luau", str(path)], check=True)
