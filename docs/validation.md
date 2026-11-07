# Validation record — 2026-11-06

## Observed environment

- Fedora 44 / Niri Tonic OS, x86-64 Linux.
- Niri **26.04**, Dank Material Shell **2.0.0**, GTK **4.22.5**.
- Native Wayland (`GDK_BACKEND=wayland`, active `wayland-1` socket).
- eDP panel 2880×1800, logical 1645×1028, compositor scale **1.75**.
  GTK reports integer buffer scale 2; the compositor handles fractional presentation.
- Both modern `~/.config/dms/settings.json` and legacy Dank settings exist.
  Fonts detected and rendered: **Comfortaa Semi-Bold**, **Geist Mono Semi-Bold**,
  inherited 11 pt (14.667 logical px).
- Niri and Dank use Nord and 12 px rounding; Niri gaps are 12 px.

GTK development packages were absent on the immutable host. Compilation used an
isolated Fedora 44 build environment with `gcc`, `gtk4-devel`, Rust 1.99.1 and the
checked-in Cargo.lock; the resulting binary ran directly on the host compositor.
No Niri, Dank, MIME-default or system-theme configuration was modified.

## Checks performed

### Revised interaction model

- Home is the default initial folder and opens alone, even when a previous session
  file exists. Session restoration requires an explicit configuration opt-in.
- Opening `Home → Alpha → Beta` gives `[Home, Alpha]`, then `[Beta, Alpha]`.
  Clicking Home next gives `[Beta, Home]`. The retained pane keeps its widgets,
  selection and watcher; only the opposite pane transitions.
- The sidebar occupies one of three equal grid cells. There is no titlebar, window
  close button, horizontal separator or selection outline. All outer gaps use
  the 12 px spacing token. Dialogs/menus use a lighter surface and darker buttons.
- A single click navigates folders; a double click opens files. Control-click
  toggles selection, and Control+Shift-click adds an anchored range.
- Only Ctrl+A/C/V/D/F/T/R/Escape are application keyboard commands. Delete, F2,
  Ctrl+X and the rest of the previous file-action bindings are disabled.
- The smoke test now clicks **actual popup buttons**, rather than invoking window
  actions directly: New File, New Folder, Open, Rename, Copy, Cut, Paste, Archive,
  View Archives, Restore, direct deletion and favorites. Rename uses the shared
  creation-style popup; its collision check preserves the source and destination.
  This regression test covers the popup-unparenting bug reported during review.
- Numbered paste is exercised three times into the source directory and tested
  against concurrent writers, hidden names, non-UTF-8 bytes and broken links.
- Favorite context menus detach a shortcut without deleting its target and open
  the parent in the opposite pane.
- The input scenario checks that clicking blank list space clears selection and
  that pressing Ctrl+F again removes the selected folder from favorites.
- Rows display local modification timestamps and readable sizes. Item counters
  and success notifications are hidden; errors and progress remain visible.
  Context-menu buttons have a transparent resting background.
- A compositor popup-grab regression was reproduced when removing a hovered
  favorite. The app now dismisses popovers explicitly without taking a native
  pointer grab. Post-operation refresh also keeps pane widgets alive so pointer
  gestures stay valid after paste.

### Build and desktop checks

- `cargo fmt --all -- --check`.
- `cargo clippy --all-targets --locked -- -D warnings`.
- `cargo test --locked`: **54 tests** (6 unit, 25 core, 3 Filen protocol,
  2 Google Drive protocol, 3 inline-credential tests, 15 storage tests).
- `cargo build --release --locked`.
- `desktop-file-validate data/io.github.tdesaules.NiriTonicFiles.desktop`.
- `--check-config`: resolved defaults, Nord, Comfortaa and Geist Mono.
- Real mapped GTK smoke test: listing, folder activation, two-column limit at the
  third level, active path, Back/Forward, independent close, path entry, reduced
  motion, create, rename, archive/restore, copy, external inotify refresh and
  favorite add/remove persistence using an isolated temporary config.
- The desktop acceptance scenario combines **real Wayland keyboard input**
  (`wtype`), a persistent **virtual pointer**, and **AT-SPI assertions**. It checks
  single/double clicks, modifier selection, disabled old shortcuts, Ctrl+D's
  immediate deletion, all new bindings, numbered paste, the rename dialog and the
  two favorite menu actions. File opening uses a recording application confined
  to the test fixture.
- Window screenshots taken from the running Niri compositor; Nord foreground
  contrast, fonts, borders, layout and 175% rendering inspected. This found and
  fixed a light-GTK-theme foreground inheritance issue. See the
  [native Wayland capture](nord-wayland.png).
  The [headerless creation dialog](nord-popup.png) shows the lighter background
  and darker buttons.
- Accessibility inspection found unnamed GTK internal model-menu items on this
  runtime. Context popovers use ordinary native buttons, verified by AT-SPI.

Core tests include simultaneous name collisions, a preexisting broken symlink,
recursive copy/delete without following links, changed sources, mid-copy
cancellation, cross-filesystem moves, isolated freedesktop Trash, archive/restore
collisions, numbered copies and races, permissions, vanished files, non-UTF-8 names/favorites, config precedence,
symlink-preserving config writes, association argv and plugin handshake rejection.

## Reproduce

```sh
make check
cargo build --release --locked
GDK_BACKEND=wayland ./target/release/niri-tonic-files --smoke-test
# Optional real keyboard/pointer acceptance test inside Niri.
# Requires wtype, Python gi with Atspi 2.0, and one active output.
NIRI_TONIC_SCREENSHOT=/absolute/path/nord.png \
  python3 scripts/check-wayland.py
```

The desktop test temporarily takes window focus, moves the pointer and replaces
clipboard contents; file operations are confined to a temporary Home. `WTYPE` can
specify the keyboard helper. `scripts/wayland_pointer.py` uses the compositor's
versioned virtual-pointer protocol. `NIRI_TONIC_BINARY` selects a debug build;
`NIRI_TONIC_SCREENSHOT` and `NIRI_TONIC_POPUP_SCREENSHOT` optionally save native
Niri captures. `NIRI_TONIC_FAILURE_SCREENSHOT` captures a failed desktop step.

GTK's Vulkan renderer emitted transient `VK_SUBOPTIMAL_KHR` messages while Niri
resized modal windows. Rendering and assertions still passed. `GSK_RENDERER=gl`
can be used to compare a renderer-specific issue without changing application code.

## Hardware/manual checks still needed

- Moving a window between **physical monitors with different scale factors**:
  only one output was connected during validation. GTK provides this integration,
  but mixed-DPI output migration has not been hardware-tested here.
- End-to-end **external drag/drop** with several third-party applications. GTK FileList targets
  and sources are implemented; the automated run exercised their transfer backend.
- A complete **Orca screen-reader** session, beyond the successful AT-SPI role,
  label and context-menu assertions.
- Network-mounted local directories and unresponsive FUSE mounts; worker isolation
  is implemented, but kernel-blocked I/O cannot guarantee immediate cancellation.

## Filen isolated validation

- Official SDK 0.4.2 adapter: **11 Node tests**, using injected SDK fixtures and
  offline protocol/crypto compatibility checks.
- Rust coverage includes independent sessions, secret redaction, deadlines,
  cancellation and child cleanup, recursive local/account/account transfers,
  collision numbering, read-only accounts, changed sources and URI equivalence.
- Offline mock protocol checks passed.
- Native Wayland smoke and local keyboard/pointer regression scenarios passed.
- Multi-account GUI scenario passed: recursive local → account A → account B →
  local transfers, numbered copies, rename/archive/restore/delete, failed then
  successful moves with source-deletion ordering, read-only/disabled instances,
  private remote opening and favorites persisted across application restarts.
- Staged installation/uninstallation passed for the app and optional Filen runtime;
  both the sample YAML and documented two-account YAML passed `--check-config`.
- Locked npm installation reports three low-severity upstream elliptic findings;
  no high/critical findings. Details are recorded in the adapter README.

No real Filen account or credential was used. Live authentication/2FA, encrypted
service transfers, quotas and external-client races remain unverified. Automatic
sync is not implemented.

## Google Drive integration

- Google Drive adapter: **27 offline Node tests** passed, covering OAuth
  state/PKCE, private session storage/rotation, revoked credentials, loopback
  cancellation, Drive pagination/ID aliases, native export/copy, MIME-aware
  resumable uploads, bounded reads, streaming progress and protocol limits.
- Shared Rust remote-process host passes the existing Filen protocol/deadline
  tests and new OAuth account isolation, browser environment allowlisting,
  credential redaction, private state-path selection and no-mutation-replay checks.
- Provider-aware `drive://` paths pass configuration, favorite serialization,
  account-ID isolation and traversal checks alongside existing `filen://` URIs.
  Schemes are checked against configured provider types at URI entry points.
- Duplicate-item identity aliases use validated portable names for new transfers;
  recursive copy coverage retains both distinct files. OAuth path coverage includes
  symlinked home/state ancestors without creating token files at construction.
- Native Wayland smoke and mixed-provider GUI checks passed with offline Filen
  and Google Drive account processes: recursive roundtrips, numbered copies,
  mutations, failed/successful moves, read-only/disabled accounts, downloaded
  opening and `drive://` favorites persisted across application restarts.
- OAuth uses a Desktop client and optional private refresh-token persistence.
  No Google account, real credential or live Drive file was used for validation.
- Application/Drive staged installation and uninstallation passed, including
  installed adapter syntax and binary help checks. Documented OAuth and Filen
  YAML examples passed `--check-config`.

## CI preparation — 2026-11-07

- Explicit inline credential values pass three additional Rust regressions:
  exact YAML roundtrips, redacted Debug/validation output, and private-pipe login
  for both cloud providers. The complete application suite passes 54 Rust tests.
- `--version` reports the compiled identity without requiring HOME or a display.
- Prototype Fedora 44 native AMD64 action containers built the real application
  and qualified the same archive bytes: Rust tests, 38 Node tests, offline mock
  protocol, isolated configuration and real GTK mapping under Weston passed.
  The diagnostic build embedded `1.0.0`; it was not an application release.
- Workflow syntax and all five CI-policy fixtures pass locally, including
  immutable action pins, routing and security-report separation.
- Catalog preparation [42384772496](https://github.com/tdesaules/github-actions/actions/runs/42384772496)
  and tag verification [42398778628](https://github.com/tdesaules/github-actions/actions/runs/42398778628)
  succeeded. Catalog release [1.6.0](https://github.com/tdesaules/github-actions/releases/tag/1.6.0)
  publishes the qualified native images; all 17 consumed images were verified
  anonymously accessible on GHCR with AMD64 and ARM64 variants.
- Consumer actions are pinned to catalog commit
  `ccd2be174adf4b29b71ce80cd76994ff97e1e8b8`, which removes temporary bootstraps
  and retains those published image digests. Its post-activation hosted run
  [42399138830](https://github.com/tdesaules/github-actions/actions/runs/42399138830)
  remains pending; confirm success before activation of the consumer workflow.
- The application workflow has not been pushed or executed on GitHub; the user
  chose local preparation and will configure its App/bot credentials separately.
