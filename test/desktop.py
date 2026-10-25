#!/usr/bin/env python3
"""Package layout and user integration; no real desktop/systemd mutations."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / 'target/debug/monarch-todo'


class Desktop(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir='/tmp/opencode', prefix='todo-desktop-')
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.home = self.base / 'home'
        self.home.mkdir()
        self.prefix = self.base / 'usr'
        self.data = self.prefix / 'share/monarch-todo'
        shutil.copytree(ROOT / 'noctalia', self.data / 'noctalia')
        shutil.copytree(ROOT / 'desktop', self.data / 'desktop')
        for source, target in (
            ('monarch-todo-reminders.service', 'lib/systemd/user/monarch-todo-reminders.service'),
            ('Todo.desktop', 'share/applications/Todo.desktop'),
        ):
            dest = self.prefix / target
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / 'desktop' / source, dest)
        self.bin = self.base / 'bin'
        self.bin.mkdir()
        for name, body in {
            'systemctl': 'printf "%s\\n" "$*" >>"$HOME/systemctl.log"\n',
            'noctalia': '''printf '%s\\n' "$*" >>"$HOME/noctalia.log"
case "$*" in
  'msg status') [[ ${OFFLINE:-0} == 0 ]] ;;
  'config export full')
    if [[ -f $HOME/enabled ]]; then printf '[plugins]\\nenabled = ["monarch/todo"]\\n'; else printf '[plugins]\\nenabled = []\\n'; fi ;;
  'msg plugins enable monarch/todo') touch "$HOME/enabled" ;;
  'msg plugins disable monarch/todo') rm -f "$HOME/enabled" ;;
esac
''',
        }.items():
            path = self.bin / name
            path.write_text('#!/bin/bash\n' + body)
            path.chmod(0o755)
        self.env = {**os.environ, 'HOME': str(self.home), 'MONARCH_TODO_DATA_DIR': str(self.data),
                    'PATH': str(self.bin) + ':' + os.environ['PATH'],
                    'XDG_CONFIG_HOME': str(self.home / '.config'),
                    'XDG_DATA_HOME': str(self.home / '.local/share'),
                    'XDG_STATE_HOME': str(self.home / '.local/state')}
        for key in ('MONARCH_LANGUAGE', 'NOCTALIA_DATA_HOME', 'LC_ALL', 'LC_MESSAGES'):
            self.env.pop(key, None)
        self.env['LANG'] = 'en_US.UTF-8'
        self.link = self.home / '.local/share/noctalia/plugins/monarch-todo'

    def run_cli(self, *args, ok=True, **env):
        result = subprocess.run([str(BINARY), *args], env={**self.env, **env}, text=True, capture_output=True, timeout=20)
        if ok: self.assertEqual(result.returncode, 0, result.stderr)
        else: self.assertNotEqual(result.returncode, 0)
        return result

    def test_setup_links_package_and_open_is_idempotent(self):
        self.run_cli('setup-desktop')
        self.assertTrue(self.link.is_symlink())
        self.assertEqual(self.link.resolve(), self.data / 'noctalia/monarch-todo')
        self.run_cli('panel')
        calls = (self.home / 'noctalia.log').read_text().splitlines()
        self.assertEqual(calls.count('msg plugins enable monarch/todo'), 1)
        self.assertEqual(calls[-1], 'msg panel-toggle monarch/todo:panel')
        self.assertIn('enable --now monarch-todo-reminders.service', (self.home / 'systemctl.log').read_text())
        self.assertFalse((self.home / '.local/state/monarch/todo.json').exists())
        self.assertFalse((self.home / '.local/share/applications/Todo.desktop').exists())

    def test_legacy_plugin_and_owned_entries_are_preserved_before_transition(self):
        shutil.copytree(ROOT / 'noctalia/monarch-todo', self.link)
        (self.link / 'custom.txt').write_text('user modification')
        unit = self.home / '.config/systemd/user/monarch-todo-reminders.service'
        unit.parent.mkdir(parents=True)
        shutil.copy(ROOT / 'desktop/monarch-todo-reminders.service', unit)
        desktop = self.home / '.local/share/applications/Todo.desktop'
        desktop.parent.mkdir(parents=True)
        desktop.write_text('[Desktop Entry]\nExec=monarch-launch-todo\n')
        self.run_cli('setup-desktop')
        self.assertTrue(self.link.is_symlink())
        self.assertFalse(unit.exists())
        self.assertFalse(desktop.exists())
        backups = self.home / '.local/state/monarch-todo/desktop-backups'
        self.assertEqual(next(backups.rglob('custom.txt')).read_text(), 'user modification')
        self.assertEqual(len(list(backups.glob('transition.*'))), 3)
        self.run_cli('setup-desktop')
        self.assertEqual(len(list(backups.glob('transition.*'))), 3)

    def test_missing_payload_or_unrelated_plugin_is_not_overwritten(self):
        self.link.mkdir(parents=True)
        (self.link / 'plugin.toml').write_text('id = "someone/else"\n')
        self.run_cli('setup-desktop', ok=False)
        self.assertFalse(self.link.is_symlink())
        (self.data / 'noctalia/monarch-todo/panel.luau').unlink()
        self.run_cli('setup-desktop', ok=False)
        self.assertEqual((self.link / 'plugin.toml').read_text(), 'id = "someone/else"\n')

    def test_custom_unit_survives_and_remove_keeps_user_data(self):
        unit = self.home / '.config/systemd/user/monarch-todo-reminders.service'
        unit.parent.mkdir(parents=True)
        unit.write_text('[Service]\nExecStart=/my/custom/command\n')
        self.run_cli('setup-desktop')
        self.assertTrue(unit.exists())
        state = self.home / '.local/state/monarch/todo.json'
        state.parent.mkdir(parents=True)
        state.write_text('[]')
        self.run_cli('remove-desktop')
        self.assertFalse(self.link.is_symlink())
        self.assertEqual(state.read_text(), '[]')
        self.assertTrue(unit.exists())

    def test_offline_setup_then_online_activation(self):
        self.run_cli('setup-desktop', OFFLINE='1')
        self.assertTrue(self.link.is_symlink())
        self.assertFalse((self.home / 'enabled').exists())
        self.run_cli('panel', OFFLINE='1', ok=False)
        self.run_cli('panel')
        self.assertTrue((self.home / 'enabled').exists())

    def test_packaged_replacements_are_required_before_legacy_cleanup(self):
        shutil.copytree(ROOT / 'noctalia/monarch-todo', self.link)
        (self.link / 'custom.txt').write_text('keep me')
        (self.prefix / 'lib/systemd/user/monarch-todo-reminders.service').unlink()
        self.run_cli('setup-desktop', ok=False)
        self.assertFalse(self.link.is_symlink())
        self.assertEqual((self.link / 'custom.txt').read_text(), 'keep me')

    def test_noctalia_data_override_and_remove_never_delete_custom_directory(self):
        local = self.home / 'custom-noctalia'
        self.run_cli('setup-desktop', NOCTALIA_DATA_HOME=str(local))
        link = local / 'plugins/monarch-todo'
        self.assertTrue(link.is_symlink())
        self.assertFalse(self.link.exists())
        link.unlink()
        link.mkdir()
        (link / 'custom.txt').write_text('keep custom override')
        self.run_cli('remove-desktop', NOCTALIA_DATA_HOME=str(local))
        self.assertEqual((link / 'custom.txt').read_text(), 'keep custom override')

    def test_upgrade_changes_the_linked_plugin_without_reseeding(self):
        self.run_cli('setup-desktop')
        payload = self.data / 'noctalia/monarch-todo/panel.luau'
        payload.write_text('-- upgraded package content\n')
        self.assertEqual((self.link / 'panel.luau').read_text(), '-- upgraded package content\n')

    @unittest.skipUnless((ROOT / 'target/release/monarch-todo').exists(), 'Build --release to validate install layout')
    def test_make_install_contains_the_complete_system_payload(self):
        destination = self.base / 'package'
        subprocess.run(['make', f'DESTDIR={destination}', 'PREFIX=/usr', 'install'],
                       cwd=ROOT, check=True, capture_output=True, text=True)
        for relative in ('usr/bin/monarch-todo', 'usr/share/monarch-todo/noctalia/monarch-todo/panel.luau',
                         'usr/share/monarch-todo/noctalia/monarch-todo/plugin.toml',
                         'usr/share/monarch-todo/desktop/integration.sh',
                         'usr/lib/systemd/user/monarch-todo-reminders.service',
                         'usr/share/applications/Todo.desktop'):
            self.assertTrue((destination / relative).is_file(), relative)
        self.assertIn('Exec=monarch-todo panel', (destination / 'usr/share/applications/Todo.desktop').read_text())
        self.assertFalse((destination / 'home').exists())

    def test_packaged_strings_follow_monarch_preference_without_monarch(self):
        en = json.loads(self.run_cli('strings').stdout)
        fr = json.loads(self.run_cli('strings', MONARCH_LANGUAGE='fr').stdout)
        self.assertEqual(en.keys(), fr.keys())
        self.assertEqual(fr['todo.title'], 'Tâches')
        self.assertEqual(en['todo.title'], 'Tasks')
        preference = self.home / '.config/monarch/language'
        preference.parent.mkdir(parents=True)
        preference.write_text('fr\n')
        self.assertEqual(json.loads(self.run_cli('strings').stdout), fr)
        self.assertEqual(json.loads(self.run_cli('strings', MONARCH_LANGUAGE='auto', LANG='en_US.UTF-8').stdout), en)
        self.assertEqual(json.loads(self.run_cli('strings', MONARCH_LANGUAGE='auto', LC_MESSAGES='fr_FR.UTF-8').stdout), fr)
        self.assertFalse((self.home / '.local/state/monarch/todo.json').exists())


if __name__ == '__main__': unittest.main()
