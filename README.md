# Niri Tonic Files

A minimal, English-language Rust file explorer for Linux, Wayland, **Niri** and
**Dank Material Shell**. Nord is the default theme.

[Screenshot under Niri at 175%](docs/nord-wayland.png)

## Local, Filen and Google Drive storage

- Start in **Home with one pane**. Open a folder to add a pane on the right.
  Subsequent opens replace the pane opposite the active one: left → right → left.
  Home and favorites follow this same rule.
- The active pane's title is accented. Close either pane independently; the survivor
  expands. Closing the last column shows an empty workspace, never closes the app.
- A right sidebar contains Home, persistent favorites and optional cloud accounts.
- The borderless bottom path field accepts full paths, `~/` and local file URIs.
  Non-UTF-8 paths use reversible file URIs. Favorites offer an Open Parent Folder action.
- Selection, ranges, keyboard shortcuts, context menus, clipboard and file drops.
- Create, rename, copy, cut/paste, direct deletion, archive and restore.
  Copies automatically use `name-1.ext`, `name-2.ext`, etc. **Existing objects are never replaced.**
- Extension/MIME associations override system applications; paths are argv, not shell code.
- Worker-thread I/O, progress/cancellation, live directory refresh, virtualized rows.
- Optional named Filen accounts, with local ↔ cloud and account ↔ account transfers.
  See [Filen installation and two-account configuration](docs/filen.md).
- Named Google Drive accounts with browser OAuth, private session persistence,
  and local ↔ Drive ↔ Filen transfers. See [Google Drive setup](docs/google-drive.md).

## Build and run

Requires Linux, Rust **1.85+**, GTK **4.12+**, a C linker and pkg-config.

```sh
# Fedora (use a development toolbox/distrobox on immutable systems)
sudo dnf install gcc gtk4-devel pkgconf-pkg-config desktop-file-utils
# Debian / Ubuntu, with GTK >= 4.12
sudo apt install build-essential pkg-config libgtk-4-dev desktop-file-utils

cargo build --release --locked
./target/release/niri-tonic-files ~/Documents
# Explicitly select Wayland if both display servers are available:
GDK_BACKEND=wayland ./target/release/niri-tonic-files
```

One folder argument opens one window; multiple folder arguments open multiple windows.
Local `file:///` URIs are accepted. The app uses the existing Wayland session; it
does not start or modify Niri/Dank. A missing config is fine.

```sh
make install                    # ~/.local/bin and ~/.local/share
make install PREFIX=/usr/local  # Requires suitable permissions
make uninstall
```

Ensure `~/.local/bin` is on PATH. The desktop entry advertises `inode/directory`
and can be selected in “Open With”; installation **does not set a default file manager**.
`DESTDIR` is supported for packaging. GTK handles output changes and scaling.

## Configuration

The first applicable path is used:

1. `NIRI_TONIC_FILES_CONFIG_FILE` (an explicitly empty value is an error).
2. `$XDG_CONFIG_HOME/niri-tonic-files/config.yaml`.
3. `~/.config/niri-tonic-files/config.yaml`.

XDG base directories must be absolute; invalid/empty values use the home fallback.
See [config.example.yaml](config.example.yaml) for the documented version-1 schema.

```sh
mkdir -p ~/.config/niri-tonic-files
cp config.example.yaml ~/.config/niri-tonic-files/config.yaml
niri-tonic-files --check-config
```

Invalid keys, schema versions, commands or theme values produce a descriptive
startup error on stderr; the application does not silently discard invalid settings.
Favorites are saved atomically to this YAML file; non-UTF-8 favorites use file URIs.
Saving favorites preserves
other values but reserializes YAML and removes comments. Hidden-file visibility,
sorting and animations are configured in YAML. Restart to reload settings.

Session restore is opt-in (`navigation.restore_session: true`); by default each
launch starts in Home. When enabled, panes are stored separately in
`$XDG_STATE_HOME/niri-tonic-files/session.json` (fallback `~/.local/state`).
Remote files opened in other applications use private account-scoped copies in
`$XDG_CACHE_HOME/niri-tonic-files/materialized` (fallback `~/.cache`). Local browsing
does not need a cache. Editing a downloaded copy does not automatically upload it.
Credentials accept an explicit `{value: "..."}`, environment `{env: NAME}`, or
Secret Service `{secret_service: reference}` source. Exactly one source is allowed.
See [credential sources and chezmoi/gopass templates](docs/secrets.md). Each enabled
`connectors.instances` entry has a unique ID, display name, type and authentication
references; duplicating a Filen entry with a new ID adds another account.

### Fonts and themes

With `follow_dank: true`, font discovery reads `$XDG_CONFIG_HOME/dms/settings.json`,
then the legacy `DankMaterialShell/settings.json`. It uses the interface family,
monospace family and weight. Without Dank, GTK's system font is retained. Explicit
`font`, `monospace_font` and `font_size` override discovery.

The default theme is [themes/nord.yaml](themes/nord.yaml). Copy it and set
`appearance.theme` to its absolute path for another theme. Colors, spacing, row
height and corner radius are validated and kept separate from logic.
Symbolic system icons, visible focus, native accessible controls and reduced motion
are used throughout. Set `navigation.animations: false` in YAML to disable animations.

## Interaction

There is no window header or menu bar. **One click opens a folder; two clicks open
a file.** Control-click selects without opening. Right-click a selection or empty
space for file actions. Rename uses the same headerless dialog as file/folder
creation, prefilled with the current name. Collisions are reported without replacing files.

| Shortcut | Action |
| --- | --- |
| Ctrl+A | Select all items in the active pane |
| Ctrl+C / Ctrl+V | Copy / Paste |
| Ctrl+R | Refresh the open panes, including remote connectors |
| Ctrl+D | Delete the selection directly |
| Ctrl+click | Toggle an item in the selection without opening it |
| Ctrl+Shift+click | Add the range between the selection anchor and this item |
| Ctrl+F | Toggle selected folders in favorites, or the current folder if no folders are selected |
| Ctrl+T | Add a pane showing the current folder when fewer than two are open; otherwise close the active pane |
| Ctrl+Escape | Close the application |

Text fields retain normal editing controls; Tab and arrow keys navigate widget
focus. Clicking blank space clears the selection. Rows show modification dates
and sizes; folder sizes describe the directory entry, not a recursive total.
Hover over a truncated name to see it in full.

Favorite rows have their own right-click menu: **Remove from Favorites** detaches
the shortcut, and **Open Parent Folder** navigates to its parent. Neither operation
deletes the favorite's target.

Drops copy files onto a directory row or into a column, including between windows
and compatible applications. Use Cut/Paste for moves. External clipboard
imports copy; an in-app cut/paste moves. Editing the path uses normal text-editing
shortcuts, not file-operation shortcuts.

## Copies, deletion and archives

Copy/Paste and file drops choose the first unused name: `report.txt`, then
`report-1.txt`, `report-2.txt`, etc. The suffix precedes the final extension.
Names without extensions and directory names receive a trailing suffix;
`.env` becomes `.env-1`, and `photos.v1` as a directory becomes `photos.v1-1`.
This also works when copying into the source folder. Rename, move and archive
collisions still stop rather than replacing data.

`Archive` moves an item to its parent's **`.archive/<original-name>`**. Use **View
Archives** even when hidden files are off, select an item, then **Restore from
Archive**. Restoring moves it to the archive directory's parent. Collisions stop
the operation; rename one item and retry. `.archive` cannot be a symlink.

**Delete** in the context menu and **Ctrl+D** perform permanent deletion
immediately, without a confirmation dialog. The Delete key is disabled.
Archive restore is available in the app and is distinct from restoring items
from a provider's trash.

Progress reports bytes/items and phase. Cancellation is cooperative, between copy
chunks or items; completed items remain. Permanent recursive deletion may already
have deleted some children when cancelled. A single atomic rename or Trash handoff
cannot be interrupted. Closing during a job keeps the window open until completion
or cancellation. See [architecture](docs/architecture.md) for operation guarantees.

## Development and scope

The six-job GitHub workflow prepares native AMD64/ARM64 binaries, qualifies the
exact archives, and publishes stable releases from annotated preparation tags.
See [CI setup and first release](docs/ci-release.md) for App/bot credentials,
the explicit initial 1.0.0 release, artifact retention and platform requirements.

See [testing](docs/validation.md) for local checks, desktop scenarios and validation
limits, [architecture](docs/architecture.md) for implementation details, and
[connectors](docs/connectors.md) for the storage contract and process protocol.

Cloud connectors are covered by offline tests; live-account validation remains
outstanding. Google Drive supports My Drive and DOCX/XLSX/PPTX exports, not shared
drives. Automatic sync, thumbnails and indexed search are not implemented.

Licensed under [GPL-3.0-only](LICENSE).
