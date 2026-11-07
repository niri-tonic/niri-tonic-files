PREFIX ?= $(HOME)/.local
CARGO ?= cargo
NPM ?= npm
PYTHON ?= python3
APP_ID = io.github.tdesaules.NiriTonicFiles
FILEN_DIR = $(DESTDIR)$(PREFIX)/share/niri-tonic-files/plugins/filen
DRIVE_DIR = $(DESTDIR)$(PREFIX)/share/niri-tonic-files/plugins/google-drive

.PHONY: build check check-ci filen check-filen check-google-drive install-plugin-sources install-filen install-google-drive install uninstall

build:
	$(CARGO) build --release --locked

filen:
	$(NPM) ci --prefix plugins/filen --omit=dev --ignore-scripts

check-filen:
	$(NPM) test --prefix plugins/filen

check-google-drive:
	$(NPM) test --prefix plugins/google-drive

check-ci:
	$(PYTHON) -m unittest discover -s .github/scripts -v

install-plugin-sources:
	install -d "$(FILEN_DIR)"
	install -m644 plugins/filen/*.mjs plugins/filen/package*.json plugins/filen/LICENSE* plugins/filen/README.md "$(FILEN_DIR)/"
	install -d "$(DRIVE_DIR)"
	install -m644 plugins/google-drive/*.mjs plugins/google-drive/package*.json plugins/google-drive/LICENSE* plugins/google-drive/README.md "$(DRIVE_DIR)/"

install-google-drive: install-plugin-sources

install-filen: filen install-plugin-sources
	rm -rf "$(FILEN_DIR)/node_modules"
	cp -a plugins/filen/node_modules "$(FILEN_DIR)/node_modules"

check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --all-targets --locked -- -D warnings
	$(CARGO) test --locked
	desktop-file-validate data/$(APP_ID).desktop

install: build install-plugin-sources
	install -Dm755 target/release/niri-tonic-files "$(DESTDIR)$(PREFIX)/bin/niri-tonic-files"
	install -Dm644 data/$(APP_ID).desktop "$(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop"
	install -Dm644 data/$(APP_ID).svg "$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg"
	install -Dm644 config.example.yaml "$(DESTDIR)$(PREFIX)/share/doc/niri-tonic-files/config.example.yaml"
	install -Dm644 docs/filen.md "$(DESTDIR)$(PREFIX)/share/doc/niri-tonic-files/filen.md"
	install -Dm644 docs/google-drive.md "$(DESTDIR)$(PREFIX)/share/doc/niri-tonic-files/google-drive.md"
	install -Dm644 docs/secrets.md "$(DESTDIR)$(PREFIX)/share/doc/niri-tonic-files/secrets.md"
	install -Dm644 docs/ci-release.md "$(DESTDIR)$(PREFIX)/share/doc/niri-tonic-files/ci-release.md"
	install -Dm644 LICENSE "$(DESTDIR)$(PREFIX)/share/licenses/niri-tonic-files/LICENSE"
	-update-desktop-database "$(DESTDIR)$(PREFIX)/share/applications"

uninstall:
	rm -f "$(DESTDIR)$(PREFIX)/bin/niri-tonic-files"
	rm -f "$(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop"
	rm -f "$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg"
	rm -f "$(DESTDIR)$(PREFIX)/share/doc/niri-tonic-files/config.example.yaml"
	rm -f "$(DESTDIR)$(PREFIX)/share/doc/niri-tonic-files/filen.md"
	rm -f "$(DESTDIR)$(PREFIX)/share/doc/niri-tonic-files/google-drive.md"
	rm -f "$(DESTDIR)$(PREFIX)/share/doc/niri-tonic-files/secrets.md"
	rm -f "$(DESTDIR)$(PREFIX)/share/doc/niri-tonic-files/ci-release.md"
	rm -f "$(DESTDIR)$(PREFIX)/share/licenses/niri-tonic-files/LICENSE"
	rm -rf "$(FILEN_DIR)"
	rm -rf "$(DRIVE_DIR)"
	-update-desktop-database "$(DESTDIR)$(PREFIX)/share/applications"
