# CI and binary releases

The single workflow is `.github/workflows/main.yml`, named `main`, with exactly
six jobs: `check`, `lint`, `build`, `test`, `release`, and `maintenance`.
Operations use dedicated Docker actions from `tdesaules/github-actions`; checkout,
artifact transport and GitHub App tokens use the pinned official actions.
Catalog actions are pinned to `ccd2be174adf4b29b71ce80cd76994ff97e1e8b8`, using
the native image digests published in catalog release `1.6.0`.
See [validation evidence](validation.md#ci-preparation--2026-11-07) for the
post-bootstrap hosted qualification status before activating this workflow.

## Routing

| Event | Behavior |
| --- | --- |
| Branch push | Select checks from the Git diff; build and test affected application code |
| Documentation-only push | Run the applicable documentation checks, without compiling binaries |
| Manual `prepare` on `main` | Compute the release version, build both native platforms, qualify, then prepare metadata and a tag |
| Stable tag push | Retrieve and retest the exact prepared artifacts; publish the GitHub Release without rebuilding |
| Schedule or manual `maintenance` | Run Renovate, link checks and advisory dependency rescans independently |

Manual runs on another branch fail explicitly. Tags must be stable SemVer without
a `v` prefix. Pull requests do not trigger this workflow. Required selected phases
must succeed; a skipped phase is accepted only when the plan declared it empty.

## What is checked

- Rust formatting, Clippy and all Rust tests with the committed Cargo lockfile.
- Filen's locked npm installation and offline tests; Google Drive's offline tests.
- Python syntax and the mock connector protocol check.
- Workflow, YAML, TOML, Markdown and desktop-entry validation as applicable.
- Repository CI-policy fixtures under `.github/scripts/`, including success,
  failure, cancellation, intentionally skipped phases and tag no-rebuild gates.
- The exact archived executable: version, isolated configuration and mapped GTK
  smoke test under a private headless Weston/D-Bus session.

No Filen or Google account credentials are used in CI. Real Niri keyboard/pointer
acceptance tests remain available locally through `scripts/check-wayland.py` and
`scripts/check-connectors.py`; the hosted GUI smoke uses Weston rather than Niri.

## Native binaries and provenance

Build and test matrices use `ubuntu-24.04` (AMD64) and `ubuntu-24.04-arm` (ARM64).
The Docker actions provide the native Fedora 44 compiler/GTK environment; Ubuntu
is only the runner host. Binaries require a **Fedora 44-compatible glibc and GTK 4**
environment. This is not a claim of compatibility with older Ubuntu or Debian.

Release assets are:

```text
niri-tonic-files-X.Y.Z-linux-amd64.tar.gz
niri-tonic-files-X.Y.Z-linux-arm64.tar.gz
SHA256SUMS
release-evidence.json
```

Each archive contains `bin/niri-tonic-files`, desktop integration, configuration
examples, documentation/licenses and cloud-adapter sources/lockfiles. Node.js 22+
is needed for cloud connectors. Filen also requires `npm ci --omit=dev
--ignore-scripts` in its installed plugin directory. No personal configuration,
password or cached account session is included.

Build emits an untested source/version/platform-bound receipt. Test verifies its
checksum, executes the candidate, rechecks integrity, and emits `tested: true`
alongside **the same archive bytes**. Security reports and extracted scan payloads
are separate from the qualification artifact.

Qualified artifacts have attempt-specific names and are never overwritten by a
later workflow attempt. Verification reads the recorded attempt's status rather
than the mutable latest attempt of that run.

Preparation binds both immutable GitHub artifact IDs and hashes in an annotated
tag. Its release commit changes only Cargo version metadata and `CHANGELOG.md`.
The original source revision and final metadata revision are recorded separately.
`NIRI_TONIC_RELEASE_VERSION` embeds the planned version before qualification;
`--version` and About report that identity. Tag runs test the original source and
the prepared executable, not a newly built release executable.

## One-time GitHub setup

In the application's repository settings, configure:

| Kind | Name | Purpose |
| --- | --- | --- |
| Variable | `RELEASE_APP_ID` | ID of the shared `niri-tonic-bot` GitHub App |
| Secret | `RELEASE_APP_PRIVATE_KEY` | That App's private key |

Install the shared App on this repository with **Contents, Pull requests, Issues
and Workflows: write**, and **Actions: read**. Release and maintenance jobs each
generate their own temporary installation token from the same App credentials.
Its release token pushes the preparation commit and tag so the tag push
can trigger a new workflow. A tag pushed using the ordinary `GITHUB_TOKEN` would
normally not trigger that run.

For Renovate's supported GitHub App configuration, also grant **Commit statuses
and Checks: read/write**, **Administration and Dependabot alerts: read**, and
organization **Members: read**. These are the additional permissions listed in
the [Renovate GitHub App guide](https://docs.renovatebot.com/modules/platform/github/#running-as-a-github-app).
Merge queues permission is only needed if merge queues are enabled; this workflow
does not enable them. Approve updated permissions on the App installation.

The application repository is `niri-tonic/niri-tonic-files`. Install/authorize
the release App for the **niri-tonic organization** and this repository. The
workflow derives repository and owner from GitHub's context, so release and
Renovate operations target the new organization automatically. Bot credentials
must also be authorized for that organization (including any approval or SSO
requirements). Repository-level settings and organization-level secret access
are separate; check both after a repository transfer.

The shared actions remain in `tdesaules/github-actions`, with their images under
`ghcr.io/tdesaules/github-actions`. The organization's Actions policy must allow
that catalog and the pinned official actions. Transferring the application does
not require renaming its existing GTK/desktop ID: keeping that stable preserves
installed desktop integration and compositor rules.

Renovate uses the shared App's installation token; no separate bot account or
`RENOVATE_TOKEN` secret is needed. Issues permission enables its Dependency
Dashboard. Disable duplicate hosted Renovate App execution for this repository.
Missing credentials fail the
selected functional operation instead of reporting a skipped operation as success.

Enable Actions and permit the pinned catalog/official actions. Direct pushes to
`main` must remain possible for release preparation; keep force-push protection
in repository settings. The person pushing workflow source needs an identity
permitted to update workflows (SSH repository access or an appropriately scoped
HTTPS credential), separate from the job and release-App tokens.

## First official release

Development remains at `0.1.0` without automatic releases.

1. Push the prepared application commits and workflow when ready.
2. Configure the App and bot settings above.
3. Open the repository's **Actions → main → Run workflow**.
4. Select branch `main`, mode `prepare`, and enable `initial-release`.
5. The pipeline qualifies AMD64 and ARM64 artifacts, then prepares **1.0.0**.
6. The annotated tag triggers verification and GitHub Release publication.

For later releases, use `prepare` with `initial-release` disabled. `feat` bumps
minor; `fix` and `perf` bump patch; any breaking change takes precedence (major
after 1.0, minor before 1.0). Other changes alone produce an explicit no-op.
Version selection uses full commit messages independently of changelog filtering.
Notes follow Keep a Changelog with dated sections and comparison links.

Do not create release tags manually: publication requires the preparation marker
and a complete verified artifact set. An advanced `main`, missing platform, wrong
source, checksum mismatch or conflicting existing release asset stops publication.
Retries reuse matching immutable assets; they do not replace different bytes or
move an existing tag. If a preparation retry has mixed workflow attempts, rerun
all required phases to produce a coherent receipt set.
Once the tag exists, retry its publication run rather than preparing the same
version again. A later failed preparation attempt does not invalidate the
successful attempt and immutable artifact IDs recorded by that tag.

Artifacts and advisory reports are retained for 90 days. GitHub Release assets
remain downloadable afterward, but verification retries requiring expired CI
artifacts fail rather than silently rebuilding an old release.

## Advisory security and maintenance

Syft scans the exact extracted release payload. Grype reports vulnerabilities
without a severity gate; high/critical findings are highlighted in summaries.
Missing reports, scanner/database errors and findings remain visible but do not
block functional CI or publication. No signature is claimed without a configured
signing identity and successful signing evidence.

Weekly maintenance uses containerized Renovate and link checks. It also rescans
the current source dependency inventory against updated advisory data; these
reports are explicitly source-bound, not presented as scans of an old binary.
Manual maintenance defaults to a Renovate dry run; turn off `renovate-dry-run`
to allow update branches/PRs.

## Local validation

```sh
make check
make check-filen check-google-drive
python3 -m unittest discover -s .github/scripts -v
```

The policy fixtures require PyYAML. Local validation and local native container
qualification are distinct from a successful hosted run. The application workflow
is prepared locally; activation and its first release require the GitHub setup
and push described above.
