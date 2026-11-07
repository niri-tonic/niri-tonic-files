# Google Drive

Google Drive is an optional Node.js connector using the official Drive v3 HTTP
API and browser-based OAuth 2.0. Each `google_drive` instance has an independent
session and appears with its configured name and **Google Drive** in the sidebar.

## Install

Use Node.js 22 or newer and `xdg-open` to launch the default browser. This adapter
uses Node's built-in libraries; it has no npm runtime dependencies.

```sh
make install                     # Application and connector sources
# Or, for an already installed application at the same PREFIX:
make install-google-drive
```

When running `./target/release/niri-tonic-files` from the repository, the adapter
is discovered in `plugins/google-drive/`. Filen dependencies are needed only if
you also enable a Filen account.

## Create a Google OAuth client

1. In [Google Cloud Console](https://console.cloud.google.com/), select or create
   a project and enable **Google Drive API**.
2. Configure **Google Auth Platform** (OAuth consent screen): app name, support
   contact and audience. For a personal external app in Testing, add your Google
   account under test users.
3. Create an OAuth client ID of type **Desktop app**, not Web application.
4. Copy its **client ID** and **client secret**. These identify your OAuth client;
   they are not your Google account password. Configure the secret using one of
   the supported credential sources below.

The connector requests `https://www.googleapis.com/auth/drive` so it can browse
and modify existing files. The narrower `drive.file` scope cannot provide a full
file explorer over existing Drive contents. Full Drive access is a restricted
scope; distributing an external app publicly can require Google's verification.
External projects in Testing generally receive refresh tokens expiring after
seven days for this scope. Google Workspace administrators can restrict access.

## Configure accounts

Merge this into `~/.config/niri-tonic-files/config.yaml` (or its XDG/config-file
override). Keep existing Filen instances in the same `instances` list:

```yaml
version: 1
connectors:
  instances:
    - id: drive-personal
      name: Personal Drive
      type: google_drive
      enabled: true
      read_only: false
      oauth:
        client_id: YOUR_DESKTOP_CLIENT_ID.apps.googleusercontent.com
        client_secret:
          env: GOOGLE_DRIVE_CLIENT_SECRET
        persist_session: true

    - id: drive-work
      name: Work Drive
      type: google_drive
      enabled: false
      oauth:
        client_id: YOUR_DESKTOP_CLIENT_ID.apps.googleusercontent.com
        client_secret:
          env: GOOGLE_DRIVE_CLIENT_SECRET
        persist_session: true
```

Choose a unique ID for every account. You may reuse a Desktop OAuth client for
several instances, selecting the desired Google account separately in each browser
authorization. Set the second instance's `enabled` to `true` to use it.
`read_only: true` prevents modifications through the application; it does not
reduce the Google OAuth scope. Restart after configuration changes.

Without a system keyring, launch from Bash using:

```sh
read -rsp 'Google OAuth client secret: ' GOOGLE_DRIVE_CLIENT_SECRET
echo
export GOOGLE_DRIVE_CLIENT_SECRET
./target/release/niri-tonic-files
```

The secret must be available in the launching environment on subsequent launches
too, for token refresh. A Secret Service reference can be used instead if a
keyring is available: `client_secret: {secret_service: drive-desktop-client}`.
The `client_secret` field is optional for client registrations that do not require
one; use the secret supplied by Google for the Desktop client you created.

For a configuration rendered by chezmoi/gopass, you can instead use
`client_secret: {value: "YOUR_CLIENT_SECRET"}`. No environment export is then
needed. See [the template example](secrets.md) for YAML escaping and private files.

Click **Personal Drive**. The first connection opens your browser: select your
Google account and authorize the application. The callback is handled on an
ephemeral localhost port, with a random state and PKCE verification. No Google
password is collected by the explorer. Browser authorization has a five-minute
deadline. `--check-config` validates YAML without starting OAuth or connecting.

## Session storage and reconnection

With `persist_session: true`, refresh credentials are stored at:

```text
$XDG_STATE_HOME/niri-tonic-files/oauth/<instance-id>/session.json
# Default:
~/.local/state/niri-tonic-files/oauth/<instance-id>/session.json
```

This is an **unencrypted, owner-only file**, outside the YAML and caches. Treat it
as a credential and do not commit or share it. Each file is bound to its instance
and OAuth client. The running process keeps its access token in memory and renews
it as needed. A keyring is not required.

To change accounts or recover from a revoked/expired refresh token, close the
application, remove only the affected instance's `session.json`, and launch again
to authorize. Revoked credentials do not trigger repeated browser windows during
background refresh. To revoke access at Google, use your Google Account's
third-party connections settings. `persist_session: false` keeps credentials only
in memory for the current adapter process and requires browser authorization again
after it exits.

## Files and transfers

- `drive://drive-personal/` opens that instance's **My Drive** root. Paths work in
  the path field, command-line arguments, favorites and the application clipboard.
- Create, rename, copy, move, delete, archive/restore and opening files use the
  same interface as local and Filen storage. **Ctrl+R** refreshes open panes.
- Transfers work local ↔ Drive, Drive ↔ Filen, and between Drive accounts.
- Within one Drive instance, native copying retains Google document types.
  Copying Google Docs/Sheets/Slides out exports DOCX/XLSX/PPTX respectively. These
  extensions are also used when opening a private local copy in another app.
  Moving a Google document out exports it and then removes the original on success;
  Google-specific revision history, sharing settings and document features are not
  preserved by this conversion.
  Exports use Google's export size limits (currently 10 MiB); other Google-native
  document types and Drive shortcuts are not followed.
- Shared drives are outside this first version. Permissions on files visible
  through My Drive are enforced by Drive and the adapter.
- Google allows duplicate names. The adapter uses virtual, disambiguated names
  for collisions rather than selecting an arbitrary item. These are application
  paths; Drive IDs remain the authoritative identity internally.
  New copies of duplicate or escaped names use portable, disambiguated filenames
  instead of carrying the source item's identity alias into the destination.
- Opening a remote file downloads a private local copy. Editing that copy does
  not upload changes automatically.

Delete remains **permanent deletion** in the UI. The connector separately supports
Drive Trash through the storage contract. Archive/restore uses a sibling `.archive`
folder, not Google's version history or Trash restoration.

Drive has no atomic unique-name creation guarantee. Uploads use private temporary
spooling and remote staging; fresh collision checks avoid intentional replacement,
but other clients can race them. An interrupted operation can leave completed or
staged items. Refresh before retrying; ambiguous mutations are not automatically
replayed. Cross-account moves verify copied source metadata before deleting source
items, but are not distributed transactions.

## Isolated validation

```sh
cargo test --locked
make check-google-drive
python3 scripts/check-connectors.py --mixed-providers
```

The GUI scenario uses offline account processes and temporary files, not Google
or Filen. OAuth and HTTP behavior are tested with injected responses/local loopback
fixtures. No real accounts or credentials were used. Live consent, API permissions,
quotas, service behavior and cross-client races still require real-account validation.

References: [installed-app OAuth](https://developers.google.com/identity/protocols/oauth2/native-app),
[Drive scopes](https://developers.google.com/workspace/drive/api/guides/api-specific-auth),
[file exports](https://developers.google.com/workspace/drive/api/guides/manage-downloads).
