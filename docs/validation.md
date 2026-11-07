# Testing

## Local checks

Requires the [build dependencies](../README.md#build-and-run), Node.js 22+, npm,
and Python 3 with PyYAML for the workflow checks.

```sh
make check                     # Rust formatting, Clippy, tests and desktop entry
make check-ci                  # Workflow policy fixtures
make filen                     # Install locked SDK dependencies
make check-filen check-google-drive
python3 scripts/check-connectors.py --protocol-only
cargo build --release --locked
GDK_BACKEND=wayland ./target/release/niri-tonic-files --smoke-test
```

The smoke test uses temporary files and real mapped GTK widgets. Rust tests cover
configuration, navigation, collisions, cancellation, archive/restore, links,
non-UTF-8 paths and connector transfers. Adapter tests use injected SDK/HTTP
responses and local OAuth callback listeners; they do not use cloud accounts.

When changing `cliff.toml`, run `make check-changelog` with git-cliff 2.14.2 on
PATH (or set `GIT_CLIFF` to its executable). This renders first-release and
multi-release fixtures, including empty groups and filtered commits, to catch
formatting errors in generated notes before release preparation.

## Niri desktop checks

Run inside a Niri session with one active output, `wtype`, and Python gi/AT-SPI:

```sh
python3 scripts/check-wayland.py
python3 scripts/check-connectors.py
python3 scripts/check-connectors.py --mixed-providers
```

These tests take window focus, move the pointer and replace clipboard contents.
File operations use a temporary Home; cloud scenarios use local mock processes.
They cover keyboard/pointer interaction, multi-account transfers, partial moves,
read-only instances, private downloaded copies and persisted favorites.

Optional environment variables:

| Variable | Purpose |
| --- | --- |
| `NIRI_TONIC_BINARY` | Select the application binary |
| `WTYPE` | Select the keyboard helper |
| `NIRI_TONIC_SCREENSHOT` | Save a native Niri screenshot |
| `NIRI_TONIC_POPUP_SCREENSHOT` | Save the creation-dialog screenshot |
| `NIRI_TONIC_CONNECTORS_SCREENSHOT` | Save the connector-scenario screenshot |
| `NIRI_TONIC_FAILURE_SCREENSHOT` | Capture a failed desktop step |

The desktop scenarios were validated on Fedora 44, Niri 26.04, Dank Material
Shell 2.0.0 and GTK 4.22.5 at 175% scale. See the [main window](nord-wayland.png)
and [creation dialog](nord-popup.png). Use `GSK_RENDERER=gl` to investigate
renderer-specific issues.

## Validation limits

The following still need manual or live-service validation:

- Moving windows between physical monitors with different scale factors.
- External drag/drop with third-party applications.
- A complete Orca screen-reader session beyond AT-SPI assertions.
- Network-mounted directories and unresponsive FUSE mounts.
- Filen login/2FA, live encrypted transfers, quotas and external-client races.
- Google consent, live API permissions, quotas and external-client races.

## Continuous integration

See [CI and releases](ci-release.md) for native artifact qualification and
publication. Current execution results are on the repository's
[Actions page](https://github.com/niri-tonic/niri-tonic-files/actions/workflows/main.yml).
Hosted GUI checks use headless Weston; the Niri scenarios above run locally.
