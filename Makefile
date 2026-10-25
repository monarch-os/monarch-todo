# Package layout shared by distribution builds and local VM previews.
PREFIX ?= /usr
DATADIR := $(PREFIX)/share/monarch-todo

.PHONY: install
install:
	install -Dm755 target/release/monarch-todo "$(DESTDIR)$(PREFIX)/bin/monarch-todo"
	install -Dm644 noctalia/monarch-todo/plugin.toml "$(DESTDIR)$(DATADIR)/noctalia/monarch-todo/plugin.toml"
	install -Dm644 noctalia/monarch-todo/panel.luau "$(DESTDIR)$(DATADIR)/noctalia/monarch-todo/panel.luau"
	install -Dm644 desktop/integration.sh "$(DESTDIR)$(DATADIR)/desktop/integration.sh"
	install -Dm644 desktop/monarch-todo-reminders.service "$(DESTDIR)$(PREFIX)/lib/systemd/user/monarch-todo-reminders.service"
	install -Dm644 desktop/Todo.desktop "$(DESTDIR)$(PREFIX)/share/applications/Todo.desktop"
	install -Dm644 README.md "$(DESTDIR)$(PREFIX)/share/doc/monarch-todo/README.md"
	install -Dm644 doc/panel.md "$(DESTDIR)$(PREFIX)/share/doc/monarch-todo/panel.md"
	install -Dm644 LICENSE "$(DESTDIR)$(PREFIX)/share/licenses/monarch-todo/LICENSE"
