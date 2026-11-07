# Architecture

## Toolkit decision

GTK 4 through gtk-rs was chosen for native Wayland surfaces, fractional-output
handling, Pango/fontconfig, AT-SPI accessibility, desktop application integration,
virtualized lists, clipboard and drag/drop. Each pane has its own GTK stack for
horizontal replacement animations without moving the other pane.
GTK's widgets also give keyboard selection and visible focus a mature foundation.
This app uses GTK directly rather than libadwaita so its palette and geometry can
follow Nord/Dank consistently.

Observed desktop conventions: Nord background `#2e3440`, surface `#3b4252`, focus
`#81a1c1`, 12 px radii and gaps. Dank fonts are Comfortaa 600 and Geist Mono on the
validation machine. These are **discovered**, not hard-coded font requirements.

## Modules

| Module | Responsibility |
| --- | --- |
| `config` | Versioned strict YAML, path precedence, session serialization, atomic writes |
| `theme` + `themes/` | Theme validation, CSS tokens and font discovery |
| `navigation` | Pure state machine, two-column snapshots, active index, bounded history |
| `connector` | Object-safe, worker-only storage contract and plugin negotiation types |
| `local` | Local implementation, byte-preserving URI conversion, exclusive operations |
| `associations` | Extension/MIME resolution, safe argv building, GIO fallback |
| `ui/columns` | Virtualized lists, selection, watches, drag/drop, horizontal transition |
| `ui/actions` | Commands, capability gating, clipboard, job dispatch |
| `ui/dialogs` | Headerless creation prompts and app picker |
| `ui/rename` | Rename command using the shared creation-style dialog |
| `ui/shortcuts` | Exact Control-based command map and legacy-key regression checks |
| `ui/smoke` | Explicit opt-in mapped-widget integration check |

## Thread and lifetime model

GTK objects are confined to the main thread. A connector is `Send + Sync`; its
blocking methods run on workers. Small bounded channels carry results and progress
back to `glib::spawn_future_local`. No directory enumeration, recursive file
operation, application discovery or MIME query runs on the event-loop thread.
Configuration/font discovery happens before presenting any window.

Each visible column owns a cancellation context. Its worker installs an inotify
watch via `notify`, lists, filters and sorts off-thread, then delivers one snapshot.
Events are coalesced and cause a new listing. Watches are cancelled when columns
leave the current view. A generation number discards stale listing/navigation
results. If watching is unavailable, the UI says so. A vanished
directory produces an inline error, rather than silently navigating elsewhere.

`gio::ListStore<BoxedAnyObject<Entry>>` and `GtkMultiSelection` back virtualized
`GtkListView` rows. Refresh restores selection by opaque location, not display text.
Home opens in one pane by default. The first folder adds a right pane; further
opens replace the opposite pane, including opens from Home/favorites. Unchanged
panes retain their widgets, selection, scroll position and watcher. A homogeneous
three-cell grid gives the explorer two cells and the sidebar one cell. The active
pane is marked by its title rather than an outline.

Row gestures own pointer selection: Control toggles, Control+Shift adds a range
anchored by a byte-preserving location, and unmodified clicks select one item.
Folders open on a single click, files on a double click. Native ListView activation
is disabled to avoid duplicate opens or Shift-only range selection. Selection is
shown with background color, without borders.

Rename captures the selected location and opens the same headerless prompt used
for creation, prefilled with the current name. The operation runs on a worker;
name collisions report an error and preserve both existing items.

The outgoing pane content is retained only for its animation, then removed. Only
the current panes have active watches. Per-pane generation tokens reject stale
worker replies without disabling the retained pane's watcher. History stores paths, not GTK widgets, and is
bounded to 256 snapshots. Closing all columns leaves a usable empty window.

One mutating/launch job runs per window. Jobs capture their source selection and
destination before dispatch, so switching the active column cannot retarget a
running operation. Progress messages may be coalesced; completion is never dropped.
Session restoration is opt-in. When enabled, state is written on a worker on
close; after GTK exits, the process waits for that write to finish.

## File-operation semantics

- Names are checked as single path components for creation/rename. Linux paths
  remain `PathBuf`/`OsString`; only display labels are lossy. Local connector IDs
  are percent-encoded GIO file URIs, preserving arbitrary filename bytes.
- All commits use Linux `renameat2(RENAME_NOREPLACE)` through rustix. An existence
  precheck improves errors, but **the syscall**, not the precheck, prevents races.
  Unsupported no-replace semantics fail rather than use an unsafe rename fallback.
- Copies are built in an unpredictable temporary directory on the destination
  filesystem. Streaming creation uses a temporary file. Data is flushed before
  the exclusive commit; cancellation/errors leave the source intact. No directory
  merging or implicit overwriting occurs.
- `copy_unique` stages the source only once, then attempts the original destination
  and numbered siblings using the same no-replace commit. Even if another writer
  wins a candidate name, the next suffix is tried atomically. Extensions and
  non-UTF-8 bytes are preserved. The strict `copy` API remains available to callers
  that require a collision error rather than automatic naming.
- Recursive copy copies symbolic links themselves, including broken links; it
  does not traverse them. Browsing a directory symlink is supported. Permanent
  deletion removes symlinks, not their targets. Copying sockets, devices and pipes
  is rejected. Copying/moving a directory into itself, including through an alias
  of its destination parent, is rejected.
- Copy checks source metadata before and after reading. Cross-device moves copy
  and commit before deleting the source, with a tree fingerprint check before
  cleanup. A changed source is kept and the partial outcome is reported. This is
  not a filesystem snapshot or a transaction against concurrent writers.
- Cross-device cleanup is non-cancellable once begun: at this stage the destination
  exists. A cleanup error explicitly reports that the copy completed and the
  original may be partially retained. Same-filesystem moves are atomic renames.
- Ordinary permission bits are copied; setuid/setgid bits are not. Ownership,
  ACLs, xattrs, hard-link topology, sparse extents and timestamps are not replicated
  by this MVP. It is not a backup utility.
- Batch operations stop at the first error and report the number completed.
  Earlier completed items remain. A collision never prompts for silent replacement.
- `.archive` is a real sibling directory. Archives retain the original basename;
  collision handling and restore use the same exclusive move as rename. No manifest
  or secret sidecar is needed to recover an object from its immediate archive.
- The current UI's Delete command performs permanent deletion directly, as does
  Ctrl+D. The separate storage-level Trash method delegates to GIO/freedesktop.
  Deletion can be partially completed if cancelled. There is no general Undo.

Operations assume a normal user-owned filesystem tree. Metadata checks reduce
ordinary concurrent-edit problems; they do not implement descriptor-relative
containment against an adversary replacing ancestors during every syscall.

## Associations

Priority is lowercase extension, exact MIME, MIME family (`image/*`), then the
system's GIO default application. Configured commands are argv arrays; absolute
paths are appended directly to `Command::args`. There is no implicit shell,
placeholder interpolation or quoting parser. Several files for one configured
application are grouped into one invocation. System defaults are grouped by MIME
and launched with GIO's desktop-entry machinery. Open With discovers installed
applications off-thread and changes no global association.

## Accessibility and integration

Native list/selection/text/button controls expose AT-SPI roles and actions. Controls
have labels/tooltips; focused buttons use the theme's focus color and the
borderless path entry changes background on focus. GTK handles keyboard navigation and output migration.
App animations respect both the configuration and GTK's animation setting.
Context-menu buttons keep a resolved `gio::Action` and dispatch it after closing
the popup. They do not rely on widget ancestry, since popup dismissal unparents
the menu before GTK's default button action would otherwise run. Popovers use no
compositor-level pointer grab; outside clicks, focus loss and Escape dismiss them
explicitly. This avoids a Niri pointer-confinement regression when a favorite's
anchor is removed after its popup closes.
The application and creation dialogs are undecorated. A single spacing token
controls outer margins and pane gaps; menus/dialogs use the raised surface and
darker buttons.
`LANGUAGE=en` is set before GTK starts so toolkit-owned dialogs are also English.
Multi-monitor and assistive-technology acceptance checks are listed in validation.
