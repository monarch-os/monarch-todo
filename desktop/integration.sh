#!/bin/bash
# Runs as the desktop user, never as a package-manager/root hook.
set -euo pipefail
action=${1:?Missing action}
root=${2:?Missing package data directory}
config=${XDG_CONFIG_HOME:-$HOME/.config}
data=${XDG_DATA_HOME:-$HOME/.local/share}
state=${XDG_STATE_HOME:-$HOME/.local/state}
plugin_root=${NOCTALIA_DATA_HOME:-$data/noctalia}/plugins
link="$plugin_root/monarch-todo"
source="$root/noctalia/monarch-todo"
prefix=$(realpath "$root/../..")
service=monarch-todo-reminders.service

backup() {
  local path=$1 directory
  mkdir -p "$state/monarch-todo/desktop-backups"
  directory=$(mktemp -d "$state/monarch-todo/desktop-backups/transition.XXXXXXXX")
  mv -- "$path" "$directory/"
  echo "Preserved $path in $directory" >&2
}

shell_ready() {
  noctalia msg status >/dev/null 2>&1
}

case $action in
  setup | open)
    # Confirm the replacement before moving any previously seeded files.
    [[ -f $source/plugin.toml && -f $source/panel.luau && -f $prefix/lib/systemd/user/$service && -f $prefix/share/applications/Todo.desktop ]] || {
      echo "monarch-todo desktop payload is incomplete: $root" >&2
      exit 1
    }
    mkdir -p "$plugin_root"
    changed=false
    if [[ ! -L $link ]] || [[ $(readlink -- "$link") != "$source" ]]; then
      if [[ -e $link || -L $link ]]; then
        [[ -f $link/plugin.toml ]] && grep -Eq '^id[[:space:]]*=[[:space:]]*"monarch/todo"' "$link/plugin.toml" || {
          echo "Refusing to replace an unrelated plugin at $link" >&2
          exit 1
        }
        backup "$link"
      fi
      ln -s -- "$source" "$link"
      changed=true
    fi

    # A user unit shadows the new packaged unit. Back up only recognized legacy
    # copies; leave custom units as explicit user overrides.
    old_service="$config/systemd/user/$service"
    if [[ -f $old_service && ! -L $old_service ]] &&
      grep -Fqx 'ExecStart=/usr/bin/monarch-todo reload-reminders' "$old_service" &&
      grep -Eq '^Description=Re-arm monarch-todo' "$old_service"; then
      # Verify the system-wide replacement is actually installed.
      [[ -f $prefix/lib/systemd/user/$service ]] || {
        echo "Packaged reminder unit is missing" >&2
        exit 1
      }
      systemctl --user disable "$service"
      backup "$old_service"
    fi
    old_desktop="$data/applications/Todo.desktop"
    if [[ -f $old_desktop ]] && grep -Eq '^Exec=(monarch-launch-todo|monarch-launch-tui monarch-todo)$' "$old_desktop"; then
      [[ -f $prefix/share/applications/Todo.desktop ]] || { echo "Packaged desktop entry is missing" >&2; exit 1; }
      backup "$old_desktop"
    fi
    systemctl --user daemon-reload
    systemctl --user enable --now "$service"

    # Noctalia v5 discovers user plugins, not XDG_DATA_DIRS. This symlink points
    # straight to pacman-owned files, so upgrades never depend on reseeding.
    if shell_ready; then
      [[ $changed == "false" ]] || noctalia msg config-reload
      enabled=$(noctalia config export full | python3 -c '
import sys, tomllib
print("yes" if "monarch/todo" in tomllib.loads(sys.stdin.read()).get("plugins", {}).get("enabled", []) else "no")
      ')
      if [[ $enabled != "yes" ]]; then noctalia msg plugins enable monarch/todo; fi
      [[ $action != "open" ]] || exec noctalia msg panel-toggle monarch/todo:panel
    elif [[ $action == "open" ]]; then
      echo "Noctalia is not running; start it before opening Tasks" >&2
      exit 1
    fi
    ;;
  remove)
    if shell_ready; then noctalia msg plugins disable monarch/todo; fi
    systemctl --user disable --now "$service"
    systemctl --user stop 'monarch-todo-reminder-*.timer' 'monarch-todo-reminder-*.service'
    if [[ -L $link && $(readlink -- "$link") == "$source" ]]; then rm -- "$link"; fi
    ;;
  *) echo "Unknown desktop action: $action" >&2; exit 2 ;;
esac
