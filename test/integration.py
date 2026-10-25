#!/usr/bin/env python3
"""Hermetic CLI tests: never touch the real store, systemd or notifications."""
import concurrent.futures
import datetime as dt
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/debug/monarch-todo"
MOCK = '''#!/usr/bin/python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ["MOCK_ROOT"])
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with (root / "calls").open("a") as f:
    f.write(json.dumps([name, args]) + "\\n")
if name in ("notify-send", "monarch-notification-send"):
    sys.exit(int(os.environ.get("FAIL_NOTIFY", "0")))
state = root / "timers.json"
timers = json.loads(state.read_text()) if state.exists() else {}
if name == "systemd-run":
    if os.environ.get("FAIL_SCHEDULE"): sys.exit(1)
    unit = next(a.split("=", 1)[1] for a in args if a.startswith("--unit="))
    timers[unit] = args
elif "list-units" in args:
    for unit in timers: print(unit + ".timer loaded active waiting")
elif "stop" in args:
    timers.pop(args[-1].removesuffix(".timer"), None)
state.write_text(json.dumps(timers))
'''


class Backend(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="todo-test-", dir="/tmp/opencode" if Path("/tmp/opencode").exists() else None)
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name in ("systemctl", "systemd-run", "notify-send", "monarch-notification-send"):
            path = self.bin / name
            path.write_text(MOCK)
            path.chmod(0o755)
        self.store = self.root / "todo.json"
        self.env = dict(os.environ, MONARCH_TODO_FILE=str(self.store), MOCK_ROOT=str(self.root), PATH=f"{self.bin}:{os.environ['PATH']}", TZ="Europe/Paris")

    def cli(self, *args, ok=True, env=None):
        result = subprocess.run([str(BINARY), *args], cwd=self.root, env=env or self.env, text=True, capture_output=True, timeout=20)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def tasks(self):
        return json.loads(self.cli("list", "--all", "--format", "json").stdout)

    def quick(self, text):
        return json.loads(self.cli("quick", text).stdout)

    def test_parse_is_side_effect_free(self):
        task = json.loads(self.cli("parse", "demain à 9h30 > Rendez-vous @daily !critical #(Vie privée)").stdout)
        self.assertEqual(task["priority"], "Critical")
        self.assertEqual(task["category"], "Vie privée")
        self.assertIn("09:30:00", task["reminder"])
        self.assertFalse(self.store.exists())
        self.assertFalse((self.root / "calls").exists())

    def test_crud_and_legacy_store(self):
        self.store.write_text(json.dumps([{"id": "old", "text": "Legacy", "done": False, "tags": ["Old category"]}]))
        task = self.tasks()[0]
        self.assertEqual(task["category"], "Old category")
        self.cli("update", "old", "> Literal > punctuation \\!high #(Old category)")
        self.assertEqual(self.tasks()[0]["text"], "Literal > punctuation !high")
        self.cli("done", "old")
        self.cli("status", "old", "todo")
        self.cli("rm", "old")
        self.assertEqual(self.tasks(), [])

    def test_bad_input_never_changes_store(self):
        self.quick("Keep this")
        before = self.store.read_bytes()
        for query in ("bad-date > Fail", "tomorrow > Fail @every:0d", "Fail !unknown", "Fail @daily"):
            self.cli("quick", query, ok=False)
            self.assertEqual(self.store.read_bytes(), before)
        self.cli("add", "Bad", "--due", "2026-02-30", ok=False)

    def test_concurrent_writers(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=10) as pool:
            list(pool.map(lambda i: self.cli("add", f"Task {i}"), range(30)))
        tasks = self.tasks()
        self.assertEqual(len(tasks), 30)
        self.assertEqual(len({t["id"] for t in tasks}), 30)

    def test_tracking_persists_across_processes_and_pause_resume(self):
        task = self.quick("Tracked")
        self.assertEqual(task["time_spent_ms"], 0)
        self.assertIsNone(task["started_at_ms"])
        self.cli("status", task["id"], "doing")
        first = self.tasks()[0]
        self.assertIsInstance(first["started_at_ms"], int)
        self.cli("status", task["id"], "doing")
        self.assertEqual(self.tasks()[0]["started_at_ms"], first["started_at_ms"])
        data = json.loads(self.store.read_text())
        data[0]["time_spent_ms"] = 12_000
        data[0]["started_at_ms"] = int(dt.datetime.now(dt.timezone.utc).timestamp() * 1000) - 63_000
        self.store.write_text(json.dumps(data))
        self.assertGreaterEqual(self.tasks()[0]["elapsed_ms"], 75_000)
        self.cli("update", task["id"], "> Renamed")
        self.assertEqual(self.tasks()[0]["started_at_ms"], data[0]["started_at_ms"])
        self.cli("status", task["id"], "todo")
        paused = self.tasks()[0]
        self.assertIsNone(paused["started_at_ms"])
        self.assertGreaterEqual(paused["time_spent_ms"], 75_000)
        self.assertEqual(paused["elapsed_ms"], paused["time_spent_ms"])
        self.cli("status", task["id"], "doing")
        self.assertEqual(self.tasks()[0]["time_spent_ms"], paused["time_spent_ms"])
        self.cli("done", task["id"])
        finished = self.tasks()[0]
        self.assertIsNone(finished["started_at_ms"])
        self.assertGreaterEqual(finished["time_spent_ms"], paused["time_spent_ms"])
        self.cli("status", task["id"], "todo")
        self.assertEqual(self.tasks()[0]["time_spent_ms"], finished["time_spent_ms"])
        calls = [json.loads(line)[0] for line in (self.root / "calls").read_text().splitlines()]
        self.assertNotIn("systemd-run", calls, "Tracking must not create reminder timers")

    def test_tracking_cancellation_and_recurrence_reset(self):
        task = self.quick("tomorrow at 9am > Recurring @daily")
        self.cli("status", task["id"], "doing")
        data = json.loads(self.store.read_text())
        reminder = data[0]["reminder"]
        data[0]["time_spent_ms"] = 42_000
        data[0]["started_at_ms"] -= 5_000
        self.store.write_text(json.dumps(data))
        self.cli("status", task["id"], "cancelled")
        cancelled = self.tasks()[0]
        self.assertIsNone(cancelled["started_at_ms"])
        self.assertGreaterEqual(cancelled["time_spent_ms"], 47_000)
        self.assertEqual(cancelled["reminder"], reminder)
        self.cli("status", task["id"], "todo")
        self.cli("done", task["id"])
        successor = next(t for t in self.tasks() if t["id"] != task["id"])
        self.assertEqual(successor["time_spent_ms"], 0)
        self.assertIsNone(successor["started_at_ms"])

    def test_legacy_doing_tasks_do_not_invent_tracking_history(self):
        self.store.write_text(json.dumps([{"id": "old", "text": "Old", "status": "doing", "created": "2000-01-01"}]))
        self.assertEqual(self.tasks()[0]["elapsed_ms"], 0)
        self.cli("status", "old", "doing")
        self.assertIsInstance(self.tasks()[0]["started_at_ms"], int)
        self.assertEqual(self.tasks()[0]["time_spent_ms"], 0)

    def test_concurrent_start_is_idempotent(self):
        task = self.quick("Parallel starts")
        self.cli("status", task["id"], "doing")
        started = self.tasks()[0]["started_at_ms"]
        with concurrent.futures.ThreadPoolExecutor(max_workers=5) as pool:
            list(pool.map(lambda _: self.cli("status", task["id"], "doing"), range(10)))
        self.assertEqual(self.tasks()[0]["started_at_ms"], started)

    def test_timer_updates_and_completion_cancel(self):
        task = self.quick("+1h > Later !high")
        self.cli("reminder", task["id"], "+2h")
        timers = json.loads((self.root / "timers.json").read_text())
        self.assertEqual(len(timers), 1)
        args = next(iter(timers.values()))
        self.assertIn("--expected", args)
        self.assertIn("--timer-property=AccuracySec=1s", args)
        self.assertIn("--property=ExitType=cgroup", args)
        self.assertIn(f"--setenv=MONARCH_TODO_FILE={self.store}", args)
        self.cli("done", task["id"])
        self.assertEqual(json.loads((self.root / "timers.json").read_text()), {})

    def test_reminder_remove_also_removes_recurrence(self):
        task = self.quick("tomorrow at 9am > Daily @daily")
        self.cli("reminder", task["id"], "-")
        self.assertIsNone(self.tasks()[0]["recurrence"])
        self.assertIsNone(self.tasks()[0]["reminder"])

    def test_recurring_completion_is_idempotent(self):
        task = self.quick("tomorrow at 9am > Daily @daily !critical")
        self.cli("done", task["id"])
        self.cli("done", task["id"])
        tasks = self.tasks()
        self.assertEqual(len(tasks), 2)
        next_task = next(t for t in tasks if t["id"] != task["id"])
        self.assertEqual(next_task["status"], "todo")
        self.assertEqual(next_task["priority"], "Critical")
        self.assertEqual(next_task["series"], task["id"])
        self.cli("status", task["id"], "todo")
        self.cli("done", task["id"])
        self.assertEqual(len(self.tasks()), 2)

    def test_month_end_anchor_survives_multiple_completions(self):
        task = self.quick("2031-01-31T09:00:00+01:00 > Monthly @monthly")
        self.cli("done", task["id"])
        second = next(t for t in self.tasks() if t["status"] == "todo")
        self.assertTrue(second["reminder"].startswith("2031-02-28T09:00:00"))
        self.cli("update", second["id"], second["reminder"] + " > Renamed @monthly")
        self.cli("done", second["id"])
        third = next(t for t in self.tasks() if t["status"] == "todo")
        self.assertTrue(third["reminder"].startswith("2031-03-31T09:00:00"))

    def test_daily_recurrence_preserves_local_hour_across_dst(self):
        task = self.quick("2030-03-30T09:00:00+01:00 > Daily @daily")
        self.cli("done", task["id"])
        pending = next(t for t in self.tasks() if t["status"] == "todo")
        self.assertEqual(pending["reminder"], "2030-03-31T09:00:00+02:00")

    def test_bimonthly_anchor_clamps_to_each_target_month(self):
        task = self.quick('2030-12-31T09:00:00+01:00 > Bimonthly @every:2mo')
        self.cli('done', task['id'])
        february = next(t for t in self.tasks() if t['status'] == 'todo')
        self.assertTrue(february['reminder'].startswith('2031-02-28T09:00:00'))
        self.cli('done', february['id'])
        april = next(t for t in self.tasks() if t['status'] == 'todo')
        self.assertTrue(april['reminder'].startswith('2031-04-30T09:00:00'))
        self.cli('done', april['id'])
        june = next(t for t in self.tasks() if t['status'] == 'todo')
        self.assertTrue(june['reminder'].startswith('2031-06-30T09:00:00'))

    def test_priorities_sort_across_categories(self):
        self.quick("Normal #A")
        self.quick("Low !low #B")
        self.quick("High !high #C")
        self.quick("Critical ! #Z")
        self.assertEqual([t["text"] for t in self.tasks()], ["Critical", "High", "Normal", "Low"])

    def test_exclamation_suffix_and_literal_roundtrip(self):
        self.assertEqual(self.quick("Urgent!")["priority"], "Critical")
        literal = self.quick(r"> Literal \word!")
        self.assertEqual(literal["text"], "Literal word!")
        self.assertIsNone(literal["priority"])

    def test_fractional_reminder_is_not_scheduled_early(self):
        task = self.quick("2030-01-01T09:00:00.5+01:00 > Fractional")
        timers = json.loads((self.root / "timers.json").read_text())
        args = next(value for key, value in timers.items() if key.startswith("monarch-todo-reminder-" + task["id"] + "-"))
        self.assertIn("--on-calendar=2030-01-01 08:00:01 UTC", args)

    def test_unrelated_mutation_preserves_an_overdue_timer_awaiting_delivery(self):
        identifier, value = self.expired()
        date = dt.datetime.fromisoformat(value)
        seconds = int(date.timestamp())
        nanos = date.microsecond * 1000
        unit = f"monarch-todo-reminder-{identifier}-{seconds}-{nanos}"
        # Model the real systemd delay between a due timestamp and delivery.
        (self.root / 'timers.json').write_text(json.dumps({unit: ['pending delivery']}))
        self.quick('An unrelated task')
        self.assertIn(unit, json.loads((self.root / 'timers.json').read_text()))
        self.cli('reminder-fire', identifier, '--expected', value)
        self.assertEqual(len(self.notifications()), 1)

    def test_editing_snoozed_metadata_preserves_original_due_date(self):
        task = self.quick('2031-01-31T09:00:00+01:00 > Original @monthly #Work')
        self.cli('snooze', task['id'], '2031-02-02T15:00:00+01:00')
        self.cli('update', task['id'], '2031-02-02T15:00:00+01:00 > Renamed @monthly !high #Home')
        edited = self.tasks()[0]
        self.assertEqual(edited['due'], '2031-01-31')
        self.assertEqual(edited['recurrence_anchor'], task['reminder'])
        self.cli('update', task['id'], '2031-02-03T15:00:00+01:00 > Rescheduled @monthly')
        self.assertEqual(self.tasks()[0]['due'], '2031-02-03')

    def test_invalid_clock_is_not_silently_normalized(self):
        self.cli("quick", "tomorrow at 25:00 > Invalid", ok=False)
        self.cli("quick", "2000-01-01T00:00:00Z > Past", ok=False)
        self.assertFalse(self.store.exists())

    def test_relative_store_path(self):
        self.cli("quick", "Relative path", env=dict(self.env, MONARCH_TODO_FILE="relative.json"))
        self.assertEqual(json.loads((self.root / "relative.json").read_text())[0]["text"], "Relative path")

    def test_snooze_rearms_one_timer_without_resetting_tracking_or_due(self):
        task = self.quick("tomorrow at 9am > Snoozed !critical")
        self.cli("status", task["id"], "doing")
        started = self.tasks()[0]["started_at_ms"]
        self.cli("snooze", task["id"], "+5m")
        snoozed = self.tasks()[0]
        self.assertNotEqual(snoozed["reminder"], task["reminder"])
        self.assertEqual(snoozed["due"], task["due"])
        self.assertEqual(snoozed["started_at_ms"], started)
        self.assertFalse(snoozed["notified"])
        self.assertEqual(len(json.loads((self.root / "timers.json").read_text())), 1)
        for invalid in ("nonsense", "yesterday", "2000-01-01T00:00:00Z"):
            before = self.store.read_bytes()
            self.cli("snooze", task["id"], invalid, ok=False)
            self.assertEqual(self.store.read_bytes(), before)
        self.cli("done", task["id"])
        self.cli("snooze", task["id"], "+5m", ok=False)

    def test_snooze_preserves_recurring_cadence_and_can_be_undone(self):
        task = self.quick("2031-01-31T09:00:00+01:00 > Monthly @monthly")
        self.cli("snooze", task["id"], "2031-02-01T15:00:00+01:00")
        self.assertEqual(self.tasks()[0]["recurrence_anchor"], task["reminder"])
        self.cli("snooze", task["id"], "2031-02-02T16:00:00+01:00")
        self.assertEqual(self.tasks()[0]["recurrence_anchor"], task["reminder"])
        self.cli("undo")
        self.assertTrue(self.tasks()[0]["reminder"].startswith("2031-02-01T15:00:00"))
        self.cli("done", task["id"])
        pending = next(t for t in self.tasks() if t["status"] == "todo")
        self.assertTrue(pending["reminder"].startswith("2031-02-28T09:00:00"))
        self.assertIsNone(pending["recurrence_anchor"])

    def test_explicit_rescheduling_replaces_a_snoozed_cadence_anchor(self):
        task = self.quick("2031-01-31T09:00:00+01:00 > Monthly @monthly")
        self.cli("snooze", task["id"], "2031-02-01T15:00:00+01:00")
        self.cli("reminder", task["id"], "2031-02-02T16:00:00+01:00")
        self.assertIsNone(self.tasks()[0]["recurrence_anchor"])
        self.cli("done", task["id"])
        pending = next(t for t in self.tasks() if t["status"] == "todo")
        self.assertTrue(pending["reminder"].startswith("2031-03-02T16:00:00"))

    def test_notification_action_is_a_safe_backend_snooze_command(self):
        identifier, date = self.expired()
        self.cli("reminder-fire", identifier, "--expected", date)
        args = self.notifications()[0][1]
        index = args.index("--action")
        self.assertEqual(args[index + 2:], [str(BINARY), "snooze", identifier, "+5m"])
        self.cli("snooze", identifier, "+5m")
        self.assertIsNotNone(self.tasks()[0]["reminder"])
        self.assertFalse(self.tasks()[0]["notified"])

    def test_undo_creation_is_persistent_and_single_use(self):
        self.quick("Existing")
        self.quick("Undo this creation")
        journal = self.root / "todo.undo.json"
        self.assertEqual(journal.stat().st_mode & 0o777, 0o600)
        self.cli("undo")
        self.assertEqual([task["text"] for task in self.tasks()], ["Existing"])
        self.assertFalse(journal.exists())
        self.cli("undo", ok=False)

    def test_undo_delete_restores_exact_task_and_position(self):
        one = self.quick("One")
        two = self.quick("Two")
        self.quick("Three")
        before = json.loads(self.store.read_text())
        self.cli("rm", two["id"])
        self.cli("undo")
        self.assertEqual(json.loads(self.store.read_text()), before)
        self.cli("set", one["id"], "--text", "Edited")
        self.cli("undo")
        self.assertEqual(json.loads(self.store.read_text()), before)

    def test_undo_preserves_unrelated_external_changes(self):
        first = self.quick("First")
        second = self.quick("Second")
        self.cli("rm", first["id"])
        data = json.loads(self.store.read_text())
        data[0]["text"] = "Changed externally"
        self.store.write_text(json.dumps(data))
        self.cli("undo")
        restored = {task["id"]: task for task in self.tasks()}
        self.assertEqual(restored[first["id"]]["text"], "First")
        self.assertEqual(restored[second["id"]]["text"], "Changed externally")

    def test_undo_conflicts_are_atomic_and_do_not_consume_history(self):
        first = self.quick("First")
        second = self.quick("Second")
        self.cli("done", first["id"])
        self.cli("done", second["id"])
        self.cli("clear")
        self.store.write_text(json.dumps([{**first, "text": "Recreated externally"}]))
        before = self.store.read_bytes()
        result = self.cli("undo", ok=False)
        self.assertIn("changed since", result.stderr)
        self.assertEqual(self.store.read_bytes(), before)
        self.assertTrue((self.root / "todo.undo.json").exists())

    def test_undo_running_completion_resumes_without_counting_the_gap(self):
        task = self.quick("Tracking")
        self.cli("status", task["id"], "doing")
        self.cli("done", task["id"])
        completed = self.tasks()[0]
        journal_path = self.root / "todo.undo.json"
        journal = json.loads(journal_path.read_text())
        journal["at_ms"] -= 600_000
        journal_path.write_text(json.dumps(journal))
        self.cli("undo")
        resumed = self.tasks()[0]
        self.assertEqual(resumed["status"], "doing")
        self.assertEqual(resumed["time_spent_ms"], completed["time_spent_ms"])
        self.assertIsInstance(resumed["started_at_ms"], int)
        self.assertLess(resumed["elapsed_ms"] - completed["time_spent_ms"], 60_000)

    def test_undo_recurring_completion_removes_untouched_successor_only(self):
        task = self.quick("tomorrow at 9am > Recurring @daily")
        self.cli("done", task["id"])
        self.assertEqual(len(self.tasks()), 2)
        self.cli("undo")
        restored = self.tasks()
        self.assertEqual(len(restored), 1)
        self.assertEqual(restored[0]["status"], "todo")
        self.assertIsNone(restored[0]["next_id"])
        self.cli("done", task["id"])
        data = json.loads(self.store.read_text())
        next(t for t in data if t["id"] != task["id"])["text"] = "Progressed externally"
        self.store.write_text(json.dumps(data))
        before = self.store.read_bytes()
        self.cli("undo", ok=False)
        self.assertEqual(self.store.read_bytes(), before)

    def test_reminder_delivery_does_not_replace_user_undo_history(self):
        identifier, date = self.expired()
        self.cli("quick", "Unrelated user action")
        self.cli("reminder-fire", identifier, "--expected", date)
        self.cli("undo")
        remaining = self.tasks()
        self.assertEqual(len(remaining), 1)
        self.assertEqual(remaining[0]["id"], identifier)
        self.assertTrue(remaining[0]["notified"])
        self.assertIsNone(remaining[0]["reminder"])

    def test_delivered_reminders_prevent_undoing_a_stale_task_image(self):
        identifier, date = self.expired()
        self.cli("set", identifier, "--text", "Changed")
        self.cli("reminder-fire", identifier, "--expected", date)
        before = self.store.read_bytes()
        self.cli("undo", ok=False)
        self.assertEqual(self.store.read_bytes(), before)

    def test_noop_and_failed_actions_preserve_previous_undo(self):
        task = self.quick("Original")
        self.cli("set", task["id"], "--text", "Changed")
        self.cli("status", task["id"], "todo")
        self.cli("snooze", task["id"], "invalid", ok=False)
        self.cli("undo")
        self.assertEqual(self.tasks()[0]["text"], "Original")

    def test_undo_after_timer_failure_recovers_the_saved_mutation(self):
        self.cli("quick", "+1h > Saved before timer failure", ok=False, env=dict(self.env, FAIL_SCHEDULE="1"))
        self.assertEqual(len(self.tasks()), 1)
        self.cli("undo")
        self.assertEqual(self.tasks(), [])

    def test_undo_refuses_a_journal_whose_store_write_never_committed(self):
        task = self.quick("Old value")
        self.cli("set", task["id"], "--text", "New value")
        journal = json.loads((self.root / "todo.undo.json").read_text())
        self.store.write_text(json.dumps([journal["changes"][0]["before"]]))
        before = self.store.read_bytes()
        self.cli("undo", ok=False)
        self.assertEqual(self.store.read_bytes(), before)

    def test_concurrent_undo_consumes_history_only_once(self):
        self.quick("Keep")
        self.quick("Undo")
        def undo(_):
            return subprocess.run([str(BINARY), "undo"], cwd=self.root, env=self.env,
                                  capture_output=True, text=True, timeout=20).returncode
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            codes = list(pool.map(undo, range(2)))
        self.assertEqual(sorted(codes), [0, 1])
        self.assertEqual([t["text"] for t in self.tasks()], ["Keep"])

    def test_undo_restored_overdue_reminder_is_delivered_immediately(self):
        identifier, _ = self.expired()
        self.cli("reminder", identifier, "+1h")
        self.cli("undo")
        self.assertEqual(len(self.notifications()), 1)
        self.assertTrue(self.tasks()[0]["notified"])
        self.assertIsNone(self.tasks()[0]["reminder"])

    def test_editing_a_snoozed_task_in_another_offset_preserves_cadence(self):
        task = self.quick("2031-01-31T09:00:00+01:00 > Monthly @monthly")
        self.cli("snooze", task["id"], "2031-02-01T15:00:00+01:00")
        data = json.loads(self.store.read_text())
        utc = dt.datetime.fromisoformat(data[0]["reminder"]).astimezone(dt.timezone.utc).isoformat()
        data[0]["reminder"] = utc
        self.store.write_text(json.dumps(data))
        self.cli("update", task["id"], utc + " > Renamed @monthly")
        self.assertEqual(self.tasks()[0]["recurrence_anchor"], task["reminder"])
        self.cli("done", task["id"])
        pending = next(t for t in self.tasks() if t["status"] == "todo")
        self.assertTrue(pending["reminder"].startswith("2031-02-28T09:00:00"))

    def test_single_category_suffix_and_reject_multiple(self):
        parsed = json.loads(self.cli("parse", "Task #(Vie privée) !high").stdout)
        self.assertEqual(parsed["text"], "Task")
        self.assertEqual(parsed["category"], "Vie privée")
        self.assertNotIn("categories", parsed)
        self.assertEqual(parsed["priority"], "A")
        task = self.quick("Task #Client")
        self.assertEqual(task["category"], "Client")
        before = self.store.read_bytes()
        for query in ("Task #Travail #Client", "Task #Work #Work", "Task #(Vie privée) #Client"):
            self.cli("quick", query, ok=False)
            self.assertEqual(self.store.read_bytes(), before)
        self.assertEqual(len(json.loads(self.cli("list", "--category", "Client", "--format", "json").stdout)), 1)
        self.assertIn("#Client", self.cli("list").stdout)

    def test_cli_category_replace_clear_and_reject_repeated_flags(self):
        self.cli("add", "Task", "--category", "Work")
        task = self.tasks()[0]
        self.cli("set", task["id"], "--category", "Client")
        self.assertEqual(self.tasks()[0]["category"], "Client")
        before = self.store.read_bytes()
        self.cli("add", "Other", "--category", "Work", "--category", "Home", ok=False)
        self.cli("set", task["id"], "--category", "-", "--category", "Admin", ok=False)
        self.assertEqual(self.store.read_bytes(), before)
        self.cli("set", task["id"], "--category", "-")
        self.assertNotIn("categories", self.tasks()[0])
        self.assertIsNone(self.tasks()[0]["category"])
        self.assertEqual(len(json.loads(self.cli("list", "--category", "", "--format", "json").stdout)), 1)

    def test_multi_category_import_keeps_first_and_backs_up_original_data(self):
        self.store.write_text(json.dumps([
            {"id": "old", "text": "Old", "status": "todo", "category": " Work ", "tags": ["Work", "Home", "", "Admin"]},
            {"id": "tags", "text": "Tags", "done": False, "tags": ["One", "Two"]},
            {"id": "clear", "text": "Clear", "status": "todo", "category": "Stale", "categories": []},
        ]))
        original = self.store.read_bytes()
        tasks = {t["id"]: t for t in self.tasks()}
        self.assertEqual(tasks["old"]["category"], "Work")
        self.assertEqual(tasks["tags"]["category"], "One")
        self.assertIsNone(tasks["clear"]["category"])
        backup = self.root / "todo.before-single-category.json"
        self.assertEqual(backup.read_bytes(), original)
        self.assertEqual(backup.stat().st_mode & 0o777, 0o600)
        self.cli("set", "old", "--text", "Renamed")
        self.assertEqual(next(t for t in self.tasks() if t["id"] == "old")["category"], "Work")
        self.assertTrue(all("categories" not in t and "tags" not in t for t in json.loads(self.store.read_text())))
        self.assertEqual(backup.read_bytes(), original)

    def test_category_edit_preserves_time_and_recurrence(self):
        task = self.quick("tomorrow at 9am > Task @daily #Work")
        self.cli("status", task["id"], "doing")
        started = self.tasks()[0]["started_at_ms"]
        self.cli("update", task["id"], task["reminder"] + " > Renamed @daily #(Vie privée)")
        edited = self.tasks()[0]
        self.assertEqual(edited["category"], "Vie privée")
        self.assertEqual(edited["started_at_ms"], started)
        self.assertEqual(edited["recurrence"], task["recurrence"])
        self.cli("done", task["id"])
        successor = next(t for t in self.tasks() if t["id"] != task["id"])
        self.assertEqual(successor["category"], "Vie privée")
        self.cli("undo")
        self.assertEqual(len(self.tasks()), 1)
        self.assertEqual(self.tasks()[0]["category"], "Vie privée")

    def test_multi_category_undo_snapshots_remain_usable_and_are_backed_up(self):
        task = self.quick("Task #Work")
        self.cli("set", task["id"], "--text", "Changed")
        data = json.loads(self.store.read_text())
        data[0]["categories"] = ["Work", "Home"]
        self.store.write_text(json.dumps(data))
        path = self.root / "todo.undo.json"
        history = json.loads(path.read_text())
        for change in history["changes"]:
            for field in ("before", "after"):
                if change[field] is not None:
                    change[field]["categories"] = ["Work", "Home"]
        path.write_text(json.dumps(history))
        original_history = path.read_bytes()
        self.cli("undo")
        self.assertEqual(self.tasks()[0]["text"], "Task")
        self.assertEqual(self.tasks()[0]["category"], "Work")
        self.assertEqual((self.root / "todo.undo.before-single-category.json").read_bytes(), original_history)

    def test_category_parentheses_and_backslashes_roundtrip(self):
        task = self.quick(r"Task #(Client \(France\))")
        self.assertEqual(task["category"], "Client (France)")
        self.cli("update", task["id"], r"> Renamed #(C:\\Users)")
        self.assertEqual(self.tasks()[0]["category"], "C:\\Users")
        before = self.store.read_bytes()
        for query in ("Task #(Unclosed", "Task #()", "Task #(Name)extra"):
            self.cli("quick", query, ok=False)
            self.assertEqual(self.store.read_bytes(), before)

    def test_single_category_backup_never_overwrites_a_different_snapshot(self):
        self.store.write_text(json.dumps([{"id":"one", "text":"Task", "status":"todo", "categories":["Work","Home"]}]))
        self.tasks()
        first = (self.root / "todo.before-single-category.json").read_bytes()
        self.store.write_text(json.dumps([{"id":"one", "text":"Task", "status":"todo", "categories":["Work","Client"]}]))
        self.tasks()
        self.assertEqual((self.root / "todo.before-single-category.json").read_bytes(), first)
        self.assertEqual((self.root / "todo.before-single-category-1.json").read_bytes(), self.store.read_bytes())

    def test_conversion_stops_if_the_original_data_cannot_be_backed_up(self):
        self.store.write_text(json.dumps([{"id":"one", "text":"Task", "status":"todo", "categories":["Work","Home"]}]))
        original = self.store.read_bytes()
        (self.root / "todo.before-single-category.json").mkdir()
        self.cli("set", "one", "--text", "Changed", ok=False)
        self.assertEqual(self.store.read_bytes(), original)

    def test_bulk_undo_preserves_order_and_rejects_duplicate_affected_ids(self):
        original = [{"id": str(i), "text": f"Task {i}", "status": "done" if i % 2 else "todo"}
                    for i in range(500)]
        self.store.write_text(json.dumps(original))
        self.cli("clear")
        self.assertEqual(len(self.tasks()), 250)
        self.cli("undo")
        self.assertEqual([t["id"] for t in json.loads(self.store.read_text())], [t["id"] for t in original])
        self.cli("set", "0", "--text", "Changed")
        current = json.loads(self.store.read_text())
        current.append(dict(current[0]))
        self.store.write_text(json.dumps(current))
        before = self.store.read_bytes()
        self.cli("undo", ok=False)
        self.assertEqual(self.store.read_bytes(), before)

    def expired(self, recurring=False):
        task = self.quick("+1h > Notify" + (" @daily" if recurring else ""))
        data = json.loads(self.store.read_text())
        data[0]["reminder"] = (dt.datetime.now(dt.timezone.utc) - dt.timedelta(days=3)).isoformat()
        self.store.write_text(json.dumps(data))
        return task["id"], data[0]["reminder"]

    def notifications(self):
        return [json.loads(line) for line in (self.root / "calls").read_text().splitlines() if json.loads(line)[0] == "monarch-notification-send"]

    def test_missed_reminders_fire_once(self):
        self.expired()
        self.cli("reload-reminders")
        self.cli("reload-reminders")
        self.assertEqual(len(self.notifications()), 1)
        self.assertIn("-g", self.notifications()[0][1])
        self.assertIsNone(self.tasks()[0]["reminder"])

    def test_recurring_delivery_keeps_anchor_and_skips_missed(self):
        identifier, _ = self.expired(recurring=True)
        self.cli("reload-reminders")
        self.cli("reload-reminders")
        self.assertEqual(len(self.notifications()), 1)
        self.assertIsNotNone(self.tasks()[0]["reminder"])
        self.cli("done", identifier)
        pending = next(t for t in self.tasks() if t["status"] == "todo")
        self.assertGreater(dt.datetime.fromisoformat(pending["reminder"]), dt.datetime.now(dt.timezone.utc))

    def test_stale_timer_and_closed_tasks_do_not_notify(self):
        identifier, date = self.expired()
        self.cli("reminder-fire", identifier, "--expected", "old")
        self.assertEqual(self.notifications(), [])
        self.cli("status", identifier, "cancelled")
        self.cli("reminder-fire", identifier, "--expected", date)
        self.assertEqual(self.notifications(), [])

    def test_notification_failure_keeps_reminder(self):
        self.expired()
        self.cli("reload-reminders", ok=False, env=dict(self.env, FAIL_NOTIFY="1"))
        self.assertFalse(self.tasks()[0]["notified"])
        self.assertIsNotNone(self.tasks()[0]["reminder"])

    def test_scheduling_failure_is_reported_but_data_is_saved(self):
        result = self.cli("quick", "+1h > Keep task", ok=False, env=dict(self.env, FAIL_SCHEDULE="1"))
        self.assertIn("Task saved", result.stderr)
        self.assertEqual(json.loads(result.stdout)["text"], "Keep task")
        self.assertEqual(len(self.tasks()), 1)


if __name__ == "__main__":
    unittest.main()
