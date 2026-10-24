--!nonstrict
-- The backend alone parses dates, owns the store, and schedules notifications.
-- This panel never reads or writes todo.json. Keep every subprocess argument quoted.
local backend = "/usr/bin/monarch-todo"
local strings = {}
local tasks, rows = {}, {}
local query, message = "", ""
local scope = "open"
local cursor, revision, epoch, request = 1, 0, 0, 0
local selectedTaskId, pendingFocusId = nil, nil
local reopenMessage = ""
local opened, busy, creating = false, false, false
local editing, deleting, preview = nil, nil, nil
local snoozing = nil
local previewQuery = nil
local parsePending, parseAt = false, 0
local lastRefresh = 0
local fetchRevision = 0
local fetching = nil
local lastTimerSecond = -1
local VISIBLE_ROWS = 3
local ROW_HEIGHT, ROW_GAP = 38, 2
local ROW_STEP = ROW_HEIGHT + ROW_GAP
-- API 33 ScrollView has an internal 8px vertical inset, separate from content
-- padding; its Lua surface does not expose that inset.
local SCROLL_INSET = 8
local VIEWPORT_HEIGHT = VISIBLE_ROWS * ROW_HEIGHT + (VISIBLE_ROWS - 1) * ROW_GAP + 2 * SCROLL_INSET
local scrollPosition, revealTaskId, dataLoaded = 0, "", false
local viewportSpan = VISIBLE_ROWS - ROW_GAP / ROW_STEP
local coordinateScale = 1
local scrollVersion = 0
local priorities = { Critical = "critical", A = "high", B = "normal", C = "low" }
-- Categorical accents from Monarch's terminal palette, distinct from urgency.
-- Cache name hashes, not colors, so dark/light changes remain live.
local categorySlots = {}
local categoryPalette = {
  dark = { "#7aa2f7", "#9ece6a", "#e0af68", "#f7768e", "#449dab", "#ad8ee6" },
  light = { "#2c5fd0", "#587539", "#8c6c3e", "#f52a65", "#007197", "#8338cc" },
}

local function t(key)
  return strings["todo." .. key] or key
end

local function quote(value)
  return "'" .. tostring(value):gsub("'", "'\\''") .. "'"
end

local function command(args)
  local parts = { quote(backend) }
  for _, arg in args do table.insert(parts, quote(arg)) end
  return table.concat(parts, " ")
end

local function openTask(task)
  return task.status == "todo" or task.status == "doing"
end

local function listMode()
  return not creating and not editing and not snoozing
end

local function queuePreview()
  preview, previewQuery = nil, nil
  request += 1
  parsePending = query ~= "" and not listMode()
  parseAt = noctalia.nowMs() + 180
end

local function leaveForm()
  creating, editing, deleting, snoozing = false, nil, nil, nil
end

local function commandError(result)
  return result.stderr ~= "" and result.stderr or t("backendFailed")
end

local function validTasks(data)
  if type(data) ~= "table" then return false end
  for index, task in data do
    if type(index) ~= "number" or type(task) ~= "table"
      or type(task.id) ~= "string" or type(task.text) ~= "string"
      or type(task.status) ~= "string" then return false end
    for _, field in { "category", "due", "reminder", "priority" } do
      if task[field] ~= nil and type(task[field]) ~= "string" then return false end
    end
    if task.recurrence ~= nil and (type(task.recurrence) ~= "table"
      or type(task.recurrence.every) ~= "number" or type(task.recurrence.unit) ~= "string") then return false end
    if task.time_spent_ms ~= nil and (type(task.time_spent_ms) ~= "number"
      or task.time_spent_ms < 0 or task.time_spent_ms >= math.huge) then return false end
    if task.started_at_ms ~= nil and (type(task.started_at_ms) ~= "number"
      or math.abs(task.started_at_ms) >= math.huge) then return false end
  end
  return true
end

local function detail(task)
  local parts = {}
  if task.status == "doing" then table.insert(parts, t("doing")) end
  local elapsed = task.time_spent_ms or 0
  local running = task.status == "doing" and type(task.started_at_ms) == "number"
  if running then elapsed += math.max(0, noctalia.nowMs() - task.started_at_ms) end
  if running or elapsed > 0 then
    local seconds = math.floor(elapsed / 1000)
    table.insert(parts, "⏱ " .. string.format("%02d:%02d:%02d", math.floor(seconds / 3600), math.floor(seconds / 60) % 60, seconds % 60))
  end
  if task.reminder then
    table.insert(parts, "♧ " .. task.reminder:gsub("T", " "):sub(1, 16))
  elseif task.due then table.insert(parts, task.due) end
  if task.recurrence then
    table.insert(parts, "↻ " .. task.recurrence.every .. " " .. t(task.recurrence.unit))
  end
  if task.priority then table.insert(parts, t(priorities[task.priority] or "normal")) end
  return table.concat(parts, " · ")
end

local function categoryColor(name)
  local slot = categorySlots[name]
  if not slot then
    local hash = 0
    for index = 1, #name do hash = (hash * 31 + string.byte(name, index)) % 2147483647 end
    slot = hash % #categoryPalette.dark + 1
    categorySlots[name] = slot
  end
  local dark = noctalia.isDarkMode == nil or noctalia.isDarkMode()
  return categoryPalette[dark and "dark" or "light"][slot]
end

local function categoryBadge(task)
  local badges = {}
  if type(task.category) == "string" and task.category ~= "" then
    local name = task.category
    local color = categoryColor(name)
    table.insert(badges, ui.row({ key = "category:" .. name, paddingH = 6, paddingV = 1,
      radius = 5, fill = color .. "33", border = color .. "88", borderWidth = 1,
    }, {
      ui.label({ text = "#" .. name, maxWidth = 140, maxLines = 1,
        fontSize = 11, fontWeight = "medium", color = "on_surface" }),
    }))
  end
  return ui.row({ key = "categories", gap = 4, align = "center" }, badges)
end

local function previewTitle(task)
  return ui.row({ key = "title-line", gap = 8, align = "center" }, {
    ui.label({ key = "title", text = task.text, maxLines = 2, fontSize = 13,
      flexGrow = 1, color = "on_surface" }),
    categoryBadge(task),
  })
end

local function rebuild()
  rows = {}
  for _, task in tasks do
    local allowed = (scope == "open" and openTask(task))
      or (scope == "reminders" and openTask(task) and task.reminder ~= nil and not task.notified)
      or (scope == "done" and not openTask(task))
    local score = if query == "" or creating or editing or snoozing then 0
      else noctalia.fuzzyScore(query, task.text .. " " .. (task.category or ""))
    if allowed and score ~= nil then table.insert(rows, { task = task, score = score }) end
  end
  if query ~= "" and not creating and not editing and not snoozing then
    table.sort(rows, function(a, b) return a.score > b.score end)
  end
  cursor = math.clamp(cursor, 1, math.max(1, #rows))
end

local function visibleRange()
  local position = math.clamp(scrollPosition, 0, math.max(0, #rows - ROW_GAP / ROW_STEP - viewportSpan))
  local first = math.floor(position + ROW_GAP / ROW_STEP + 0.0001) + 1
  local last = math.ceil(position + viewportSpan - 0.0001)
  return math.min(first, math.max(1, #rows)), math.min(last, #rows)
end

local function revealSelection()
  if not rows[cursor] then revealTaskId = ""; return end
  if cursor - 1 < scrollPosition then scrollPosition = cursor - 1
  elseif cursor - ROW_GAP / ROW_STEP > scrollPosition + viewportSpan then
    scrollPosition = cursor - ROW_GAP / ROW_STEP - viewportSpan
  end
  scrollPosition = math.clamp(scrollPosition, 0, math.max(0, #rows - ROW_GAP / ROW_STEP - viewportSpan))
  revealTaskId = rows[cursor].task.id
end

function onListScrolled(offset, maximum, rowCount)
  if not opened or not listMode() then return end
  local oldFirst, oldLast = visibleRange()
  local maxOffset = tonumber(maximum) or 0
  local extent = #rows * ROW_STEP - ROW_GAP - VIEWPORT_HEIGHT
  local currentExtent = rowCount == nil or rowCount == #rows
  local estimate = currentExtent and extent > 0 and (maxOffset - 2 * SCROLL_INSET) / extent or coordinateScale
  -- A queued callback may describe the previous filter's extent. Keep the
  -- last usable scale rather than interpreting that as a near-zero viewport.
  if estimate >= 0.125 and estimate <= 8 then coordinateScale = estimate end
  local scale = coordinateScale
  -- Derive the host's coordinate scale from its full extent, accounting for
  -- the native inset. Callback coordinates need not be logical UI pixels.
  viewportSpan = (VIEWPORT_HEIGHT - 2 * SCROLL_INSET / scale) / ROW_STEP
  scrollPosition = math.clamp((tonumber(offset) or 0) / (ROW_STEP * scale), 0,
    math.max(0, #rows - ROW_GAP / ROW_STEP - viewportSpan))
  local first, last = visibleRange()
  local hadReveal = revealTaskId ~= ""
  revealTaskId = ""
  local oldCursor = cursor
  local firstFull = math.min(math.max(1, #rows), math.ceil(scrollPosition - 0.0001) + 1)
  local lastFull = math.min(#rows, math.floor(scrollPosition + viewportSpan + ROW_GAP / ROW_STEP + 0.0001))
  if lastFull < firstFull then firstFull, lastFull = first, math.max(first, last) end
  cursor = math.clamp(cursor, firstFull, lastFull)
  if rows[cursor] then selectedTaskId = rows[cursor].task.id end
  if first ~= oldFirst or last ~= oldLast or hadReveal or cursor ~= oldCursor then render() end
end

local function shortcut(binding, label)
  return ui.row({ gap = 4, align = "center" }, {
    ui.row({ key = "keycap", paddingH = 3, paddingV = 0, radius = 3,
      fill = "surface_variant", border = "outline/0.55", borderWidth = 1,
    }, {
      ui.label({ text = binding, fontFamily = "monospace", fontSize = 9,
        fontWeight = "medium", color = "on_surface" }),
    }),
    ui.label({ text = t("help." .. label), fontSize = 9, color = "on_surface/0.9" }),
  })
end

local function keyboardFooter()
  local rows = { ui.separator({ color = "outline/0.3", thickness = 1, spacing = 0 }) }
  local function line(items)
    table.insert(rows, ui.row({ gap = 8, align = "center" }, items))
  end
  local escape = t("keyEscape")
  if creating or editing then
    line({ shortcut("↵", "save"), shortcut("Ctrl+↵", "continue"),
      shortcut("Alt+←", "back"), shortcut(escape, "close") })
  elseif snoozing then
    line({ shortcut("↵", "snooze"), shortcut("Alt+←", "back"), shortcut(escape, "close") })
  elseif deleting then
    line({ shortcut("↵", "delete"), shortcut("Alt+←", "back"), shortcut(escape, "close") })
  else
    line({ shortcut("↑↓", "choose"), shortcut("↵", "edit"), shortcut("Ctrl+N", "new"),
      shortcut("Tab", "view"), shortcut(escape, "close") })
    line({ shortcut("Ctrl+S", "track"), shortcut("Ctrl+" .. t("keySpace"), "complete"),
      shortcut("Ctrl+M", "snooze") })
    line({ shortcut("Ctrl+Alt+Z", "undo"), shortcut("Ctrl+D", "delete"),
      shortcut("Ctrl+" .. t("keyShift") .. "+X", "cancel") })
  end
  return ui.column({ key = "hint", gap = 2 }, rows)
end

function render()
  local list = {}
  local start, lastVisible = visibleRange()
  local listMode = listMode()
  if snoozing then
    table.insert(list, ui.label({ text = t("snoozing") .. " · " .. snoozing.text, color = "primary", maxLines = 1 }))
    table.insert(list, ui.row({ gap = 6 }, {
      ui.button({ text = "+5 min", onClick = function() onSnoozePreset("+5m") end }),
      ui.button({ text = "+1 h", onClick = function() onSnoozePreset("+1h") end }),
      ui.button({ text = t("tomorrowMorning"), onClick = function() onSnoozePreset("tomorrow at 9am") end }),
    }))
    if preview and previewQuery == query then
      table.insert(list, ui.label({ text = detail(preview), color = "primary", maxLines = 1 }))
    end
  elseif creating or editing then
    table.insert(list, ui.label({ text = editing and t("editing") or t("creating"), color = "primary" }))
    table.insert(list, ui.label({ text = "tomorrow at 9am > … @daily !high #Maison", color = "on_surface/0.55", fontSize = 12 }))
    if preview and previewQuery == query then
      table.insert(list, previewTitle(preview))
      table.insert(list, ui.label({ text = detail(preview), color = "primary", maxLines = 2 }))
    end
  else
    -- One overscan row below; transparent spacers preserve the full native
    -- extent and prevent hidden text bleeding into ScrollView's top inset.
    local first = start
    local last = math.min(#rows, lastVisible + 1)
    if first > 1 then
      table.insert(list, ui.spacer({ key = "before-rows", height = (first - 1) * ROW_STEP - ROW_GAP, flexGrow = 0 }))
    end
    for index = first, last do
      local task = rows[index].task
      local selected = index == cursor
      table.insert(list, ui.row({
        key = task.id, height = ROW_HEIGHT, gap = 8, padding = 2, radius = 8, align = "center",
        fill = selected and "surface_variant" or "surface",
        onClick = function() cursor = index; revealSelection(); render() end,
      }, {
        ui.label({ text = not openTask(task) and "✓" or task.status == "doing" and "◐" or "○",
          width = 20, color = task.priority == "Critical" and "error" or "primary" }),
        ui.column({ flexGrow = 1, gap = 2 }, {
          ui.label({ key = "title", text = task.text, maxLines = 1, fontSize = 13, color = "on_surface" }),
          ui.label({ text = detail(task), maxLines = 1, fontSize = 10, color = "on_surface/0.55" }),
        }),
        categoryBadge(task),
      }))
    end
    if last < #rows then
      table.insert(list, ui.spacer({ key = "after-rows", height = (#rows - last) * ROW_STEP - ROW_GAP, flexGrow = 0 }))
    end
    if #rows == 0 then table.insert(list, ui.label({ text = t("empty"), color = "on_surface/0.55" })) end
  end
  local view = t(scope)
  if not creating and not editing and not snoozing and #rows > VISIBLE_ROWS then
    view ..= " · " .. start .. "–" .. lastVisible .. " / " .. #rows
  end
  local header = ui.row({ key = "header", gap = 6, align = "center" }, {
    ui.label({ text = t("title") .. " (" .. #rows .. ")", fontSize = 15, fontWeight = "bold", flexGrow = 1 }),
    ui.label({ key = "view", text = view, fontSize = 11, color = "on_surface_variant" }),
  })
  local versionAtRender, countAtRender = scrollVersion, #rows
  local body = ui.scroll({ key = listMode and "task-list" or "form-scroll", flexGrow = 1,
    gap = listMode and ROW_GAP or 3, paddingH = listMode and 8 or 0, paddingV = 0,
    revealKey = listMode and revealTaskId or "",
    onScroll = listMode and function(offset, maximum)
      if versionAtRender == scrollVersion then onListScrolled(offset, maximum, countAtRender) end
    end or nil,
  }, list)
  if listMode then
    -- ScrollView's intrinsic measurement can retain its initial empty height.
    -- A constrained flex wrapper gives it an exact viewport on every layout.
    body = ui.column({ key = "list-viewport", height = VIEWPORT_HEIGHT }, { body })
  end
  local content = {
    header,
    ui.input({ key = "query-" .. revision, value = query, focus = true,
      placeholder = snoozing and t("snoozePlaceholder") or (creating or editing) and t("createPlaceholder") or t("searchPlaceholder"),
      onChange = "onQueryChanged", onSubmit = "onSubmit", enabled = not busy }),
    body,
  }
  if listMode then table.insert(content, ui.spacer({ key = "list-space", flexGrow = 1 })) end
  if deleting then
    table.insert(content, ui.label({ key = "delete-confirm", text = t("deleteConfirm") .. " « " .. deleting.text .. " »", color = "error", maxLines = 2 }))
  end
  if busy or message ~= "" then
    table.insert(content, ui.label({ key = "status", visible = true, text = busy and t("saving") or message,
      color = "on_surface_variant", fontSize = 11, maxLines = 2 }))
  end
  table.insert(content, keyboardFooter())
  panel.render(ui.column({ gap = 8, flexGrow = 1 }, content))
end

local function fetch()
  if not opened or busy or fetching == fetchRevision then return end
  local generation = epoch
  fetchRevision += 1
  local version = fetchRevision
  fetching = version
  local accepted = noctalia.runAsync(command({ "list", "--all", "--format", "json" }), function(result)
    if fetching == version then fetching = nil end
    if not opened or epoch ~= generation or busy or fetchRevision ~= version then return end
    if result.exitCode ~= 0 then
      message = commandError(result)
    else
      local data = noctalia.json.decode(result.stdout)
      if validTasks(data) then
        local focus = pendingFocusId ~= nil or not dataLoaded
        local selected = pendingFocusId or (rows[cursor] and rows[cursor].task.id) or selectedTaskId
        tasks = data
        rebuild()
        for index, row in rows do
          if row.task.id == selected then cursor = index; break end
        end
        pendingFocusId = nil
        selectedTaskId = rows[cursor] and rows[cursor].task.id or nil
        dataLoaded = true
        scrollPosition = math.clamp(scrollPosition, 0, math.max(0, #rows - ROW_GAP / ROW_STEP - viewportSpan))
        if focus then revealSelection() end
      else message = t("backendFailed") end
    end
    render()
  end)
  if not accepted then
    fetching = nil
    message = t("backendFailed")
    render()
  end
end

local function reset(value, keepViewport)
  scrollVersion += 1
  query = value or ""
  revision += 1
  cursor = 1
  if not keepViewport then scrollPosition = 0 end
  revealTaskId = ""
  deleting = nil
  queuePreview()
  rebuild()
  if listMode() and not pendingFocusId then revealSelection() end
  render()
end

local function mutate(args, keepOpen)
  if busy then return end
  local wasList = listMode()
  busy = true
  fetchRevision += 1
  request += 1
  local generation = epoch
  render()
  local accepted = noctalia.runAsync(command(args), function(result)
    if not opened or epoch ~= generation then return end
    busy = false
    local saved = args[1] == "quick" and noctalia.json.decode(result.stdout) or nil
    local created = type(saved) == "table" and type(saved.id) == "string"
    if created then
      pendingFocusId, selectedTaskId = saved.id, saved.id
      scope = "open"
    end
    if result.exitCode ~= 0 then
      message = commandError(result)
      if created then
        leaveForm()
        reset("", wasList)
      end
      -- A scheduling failure may occur after a successful store write. Never
      -- auto-retry creation: retain input and show the error and refreshed list.
      fetch()
      render()
      return
    end
    if not created and (args[1] == "status" or args[1] == "update" or args[1] == "snooze") then
      pendingFocusId, selectedTaskId = args[2], args[2]
    end
    leaveForm()
    message = created and t("created") or ""
    reset("", wasList)
    if keepOpen then fetch()
    else reopenMessage = message; panel.close() end
  end, 60000)
  if not accepted then busy = false; message = t("backendFailed"); render() end
end

local function saveDraft(keepOpen)
  if query == "" then return end
  if snoozing then mutate({ "snooze", snoozing.id, query }, true)
  elseif editing then mutate({ "update", editing.id, query }, keepOpen)
  else mutate({ "quick", query }, keepOpen) end
end

function onSubmit()
  if busy then return end
  if snoozing then
    saveDraft(true)
    return
  end
  if deleting then mutate({ "rm", deleting.id }, true); return end
  if creating or editing then
    saveDraft(false)
  elseif rows[cursor] then onEdit()
  elseif query ~= "" then creating = true; reset(query) end
end

function onNew()
  if busy then return end
  if rows[cursor] then selectedTaskId = rows[cursor].task.id end
  creating, editing, snoozing, message = true, nil, nil, ""
  reset(query)
end

function onSnooze()
  if busy or not listMode() or not rows[cursor] then return end
  local task = rows[cursor].task
  if not openTask(task) then message = t("reopenFirst"); render(); return end
  snoozing, message = task, ""
  reset("")
end

function onSnoozePreset(when)
  if snoozing and not busy then mutate({ "snooze", snoozing.id, when }, true) end
end

function onUndo()
  if busy or not listMode() then return end
  mutate({ "undo" }, true)
end

function onEdit()
  if busy or not rows[cursor] then return end
  local task = rows[cursor].task
  if not openTask(task) then message = t("reopenFirst"); render(); return end
  editing, creating = task, false
  -- Escape literal suffix-looking title words so round-trips cannot reinterpret them.
  local text = task.text:gsub("([^%s]+)", function(word)
    return (word:match("^[!@#\\]") or word:sub(-1) == "!") and "\\" .. word or word
  end)
  if task.recurrence then
    local units = { minutes = "m", hours = "h", days = "d", weeks = "w", months = "mo", years = "y" }
    text ..= " @every:" .. task.recurrence.every .. units[task.recurrence.unit]
  end
  if task.priority then text ..= " !" .. priorities[task.priority] end
  if task.category then
    local escaped = task.category:gsub("\\", "\\\\"):gsub("%(", "\\("):gsub("%)", "\\)")
    text ..= " #(" .. escaped .. ")"
  end
  text = (task.reminder or "") .. " > " .. text
  reset(text)
end

function onScope()
  if busy or not listMode() then return end
  scrollVersion += 1
  scope = scope == "open" and "reminders" or scope == "reminders" and "done" or "open"
  cursor = 1
  scrollPosition, revealTaskId = 0, ""
  rebuild()
  revealSelection()
  render()
end

function onQueryChanged(value)
  scrollVersion += 1
  query = value or ""
  message, deleting = "", nil
  cursor = 1
  scrollPosition, revealTaskId = 0, ""
  -- A > is an unambiguous fast-create request; ordinary text remains a search.
  if not editing and not snoozing and query:find(">", 1, true) then creating = true end
  queuePreview()
  rebuild()
  if listMode() then revealSelection() end
  render()
end

function onFrameTick()
  if not opened or busy then return end
  local now = noctalia.nowMs()
  if parsePending and now >= parseAt then
    parsePending = false
    local generation, version, text = epoch, request, query
    local input = snoozing and (text .. " > Reminder") or text
    noctalia.runAsync(command({ "parse", input }), function(result)
      if not opened or epoch ~= generation or request ~= version then return end
      if result.exitCode == 0 then
        preview = noctalia.json.decode(result.stdout)
        previewQuery, message = text, ""
      else message = result.stderr end
      render()
    end)
  end
  if now - lastRefresh > 5000 then lastRefresh = now; fetch() end
  local second = math.floor(now / 1000)
  if second ~= lastTimerSecond then
    lastTimerSecond = second
    if listMode() then
      local start, last = visibleRange()
      for index = start, last do
        local task = rows[index].task
        if task.status == "doing" and task.started_at_ms ~= nil then render(); break end
      end
    end
  end
end

function onKey(chord, pressed)
  if not pressed or busy then return end
  if chord == "return" then onSubmit()
  elseif chord == "alt+left" then
    leaveForm()
    message = ""
    reset("")
  elseif chord == "ctrl+return" then
    if deleting then return end
    saveDraft(true)
  elseif chord == "ctrl+n" then onNew()
  elseif chord == "ctrl+e" and listMode() then onEdit()
  elseif chord == "ctrl+m" then onSnooze()
  elseif chord == "ctrl+alt+z" then onUndo()
  elseif chord == "tab" then onScope()
  elseif chord == "ctrl+r" then message = ""; fetch()
  elseif listMode() then
    if chord == "down" or chord == "ctrl+j" then cursor = math.min(#rows, cursor + 1); revealSelection()
    elseif chord == "up" or chord == "ctrl+k" then cursor = math.max(1, cursor - 1); revealSelection()
    elseif rows[cursor] then
      local task = rows[cursor].task
      if chord == "ctrl+space" then mutate({ "status", task.id, openTask(task) and "done" or "todo" }, true)
      elseif chord == "ctrl+s" then mutate({ "status", task.id, task.status == "doing" and "todo" or "doing" }, true)
      elseif chord == "ctrl+d" then deleting = task; message = ""
      elseif chord == "ctrl+shift+x" then mutate({ "status", task.id, "cancelled" }, true) end
    end
    render()
  end
end

function onOpen(context)
  epoch += 1
  opened, busy = true, false
  fetching = nil
  leaveForm()
  tasks, rows, message, scope = {}, {}, reopenMessage, "open"
  dataLoaded = false
  reopenMessage = ""
  backend = "/usr/bin/monarch-todo"
  local data = type(context) == "string" and noctalia.json.decode(context) or nil
  -- Explicit local developer backend override. Never persisted.
  if type(data) == "table" then
    if type(data.backend) == "string" and data.backend:sub(1, 1) == "/" then backend = data.backend end
  end
  local generation = epoch
  noctalia.runAsync(command({ "strings" }), function(result)
    if not opened or epoch ~= generation then return end
    if result.exitCode == 0 then strings = noctalia.json.decode(result.stdout) or {} end
    render()
  end)
  reset("")
  lastRefresh = noctalia.nowMs()
  lastTimerSecond = -1
  panel.setNeedsFrameTick(true)
  fetch()
end

function onClose()
  if not pendingFocusId then
    if editing then selectedTaskId = editing.id
    elseif snoozing then selectedTaskId = snoozing.id
    elseif not creating and rows[cursor] then selectedTaskId = rows[cursor].task.id end
  end
  opened = false
  epoch += 1
  request += 1
  panel.setNeedsFrameTick(false)
end
