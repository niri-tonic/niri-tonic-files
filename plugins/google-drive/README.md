# Google Drive adapter

GPL-3.0-only. Node.js 22+; no npm dependencies. Start with
`node plugins/google-drive/adapter.mjs`. Run isolated tests with
`npm test --prefix plugins/google-drive`. Tests use injected HTTP responses and
local ephemeral OAuth callback listeners, never Google accounts or live Drive.

## OAuth and private sessions

Enable Drive API in a Google Cloud project and create a **Desktop app** OAuth
client. The host sends the client ID and optional resolved client secret over
stdin. Initial authorization opens the system browser using `xdg-open` without
a shell. The redirect is `http://127.0.0.1:<ephemeral-port>/`; authorization uses
random state, PKCE S256, account selection, and a five-minute deadline.

The requested scope is `https://www.googleapis.com/auth/drive`, permitting
browsing existing My Drive files. `drive.file` cannot provide that experience.
The full Drive scope is **restricted**, so Google's sensitive/restricted-scope
verification rules apply to public distribution. External OAuth projects in
**Testing** issue refresh tokens that normally expire after **7 days**. Add the
intended Google accounts as test users while testing your own OAuth project.

When `persist_session` is true, the host selects an absolute per-instance
`token_path` (normally `paths.state/oauth/<instance>/session.json`). Refresh
credentials are stored **unencrypted**, outside YAML, in an owner-only `0600`
file with a private `0700` parent. Writes use an exclusive temporary file and
atomic rename. Symlinks, wrong ownership, hard-linked files and group/world
permissions are rejected. The session is bound to the client ID and instance.
Access tokens, authorization codes, and client secrets are never persisted.
With persistence disabled, credentials remain in process memory only.

A saved refresh token is reused. Revocation/expiry returns
`authentication_required`, without automatically opening a browser or retrying
interactive authorization. To deliberately sign in again, close the connector,
remove its private session file, and reconnect. Browser-launch failures and
OAuth errors are sanitized. Neither tokens nor authorization URLs are logged.

## Protocol

Version 1 JSON lines on stdin/stdout; stdout is exclusively RPC. One request at
a time, with `cancel` and `shutdown` allowed during an active request. Every
request has `protocol: 1`, integer `id`, string `op`, and object `args`.

* `hello`: `{instance, read_only}` → `{connector: instance}`.
* `login`: `{client_id, client_secret?: string, token_path, persist_session}` → null.
* `list`, `stat`: `{path}`; list returns `{entries, next: null}`.
* `read`: `{path, offset, limit}` → byte array; limit at most 1 MiB.
* `write_new`: `{path, source}`; source is an absolute owner-only regular spool.
* `create_directory`: `{path}`.
* `rename`, `move_item`, `copy`, `copy_unique`: `{path, destination}`.
  `move` is also accepted. `copy_unique` returns a Location.
* `delete`: `{path, mode: "trash" | "permanent"}`.
* `archive`, `restore`: `{path}` → Location; use a sibling `.archive` directory.
  Restore means archive restore, not restoring an item from Google's Trash.

Results and credential-free progress follow the Filen protocol. Progress is
coalesced to one frame per second during work, including heartbeats during API
waits, with monotonic accumulated bytes and committed item counts. There is at
most one pending stdout write; awaited callbacks backpressure transfer work.
Each committed file or folder in a recursive copy contributes one item. All
errors are non-retryable. Input frames are bounded at 2 MiB, output at 5 MiB. Directory
enumeration follows every page, rejects cursor loops/incomplete results, and
limits a directory to 100,000 items; an oversized RPC result fails explicitly.
Network requests have 30-second deadlines, including response-body consumption.
Cancellation, EOF, shutdown and signals abort active work and remove private
temporary exports. No operation is automatically replayed after a failed API
request, including uploads and mutations whose outcome may be ambiguous.

## Paths, formats and limitations

Locations remain instance-scoped `file:///virtual/path` URIs. Drive IDs stay
internal. Ordinary unique names stay readable. Unsafe names (slash, backslash,
controls, dot components, empty names, or names beginning with `~`) use
`~e<base64url-UTF8>`. Duplicate display names use
`~i<base64url-Drive-ID>~<virtual-name>`. The `~` namespace is escaped for actual
Drive names, so a real reserved-looking name cannot impersonate an alias.
Aliases resolve by ID **and parent**, including after a duplicate is removed.
Entry names always equal virtual path basenames. A formerly unique plain path
must be refreshed if another client introduces a duplicate.

Entries additionally return optional `transfer_name` (null for ordinary unique
safe names). Use this component rather than `name` when constructing a new
cross-provider or UI transfer destination. The exported
`transferName(component, item)` helper defines the mapping:

* An ordinary duplicate becomes `<stem>~drive-<base64url-UTF8-Drive-ID><extension>`.
  For example, alias `~iYQ~same.txt` for ID `a` maps to `same~drive-YQ.txt`.
* An escaped/unsafe underlying name becomes
  `Drive-<base64url-UTF8-Drive-ID><extension>`; for ID `c`, `a/b.txt` maps to
  `Drive-Yw.txt`. Native export extensions take precedence; ordinary files keep
  their last extension if it has no slash, backslash, or control characters.
* Other components are unchanged. All generated components avoid a leading `~`.

Same-account `copy` normalizes destination ID aliases, recursive copies use the
helper for every child, and `copy_unique` returns the actual new Location after
normalization/collision selection. New copies receive fresh Drive IDs; the
source ID suffix is merely an ordinary name. `write_new` expects the host to use
`transfer_name` and rejects unconverted ID aliases. Moves can keep the existing
ID-qualified alias. Fresh destination collisions still fail without overwrite;
`copy_unique` adds a numeric suffix before the extension.

Docs, Sheets and Slides appear with virtual `.docx`, `.xlsx`, `.pptx` extensions
(appended even if the underlying name already has an extension). Same-account
copies use `files.copy` and retain native document types. Cross-account/local
transfers export real Office-format bytes. List size may be zero; `stat` exports
to private disk and returns the exact byte count. Entry revisions always use
stable Drive metadata (ID, version, modified time, checksum), independently of
export cache presence. Source metadata is checked around export; a separate
export SHA-256 is retained internally. Exports are cached per ID/revision, up
to 16 exports of at most 10 MiB each; failed partial exports are removed. Other
Google-native types and shortcuts are explicitly unsupported. Shortcuts are
never followed. Shared drives are excluded; shared files reachable in My Drive
respect their Drive capabilities.

Ordinary files use verified HTTP byte ranges of at most 1 MiB, never whole-file
memory downloads. Uploads read private host spools in 1 MiB resumable chunks,
verify acknowledgements, and use random `.niri-transfer-*` staging names. A
staging name retains the destination extension. Known Office, PDF and image
extensions get their media content types; metadata omits `mimeType` so Drive can
detect the uploaded content. Uploads never request Google-native conversion. A
fresh collision check precedes final rename; ambiguous failures retain remote
staging/completed data rather than retrying or deleting it. Root mutations and
read-only-instance mutations are forbidden. Source revisions include ID,
version, modified time and MD5 where available, and are checked around reads
and copies.

**Drive provides no atomic unique-name guarantee.** Fresh complete listings
check destinations before mutations; post-operation checks detect concurrent
same-name items and retain both. A race can therefore return an error after a
successful creation, copy or move. Refresh before retrying. `copy_unique` is
best-effort under concurrent clients. Recursive copies can leave completed
children if a later child fails or collides.

## Official references verified for this implementation

* [Installed-app OAuth, loopback and PKCE](https://developers.google.com/identity/protocols/oauth2/native-app)
* [Refresh-token expiration / Testing](https://developers.google.com/identity/protocols/oauth2#expiration)
* [Drive downloads and 10 MB export limit](https://developers.google.com/workspace/drive/api/guides/manage-downloads)
* [Resumable uploads and 256 KiB chunk multiples](https://developers.google.com/workspace/drive/api/guides/manage-uploads)
* [Official Office export MIME types](https://developers.google.com/workspace/drive/api/guides/ref-export-formats)
* [File MIME detection and native import semantics](https://developers.google.com/workspace/drive/api/reference/rest/v3/files)
* [Create-file extension guidance](https://developers.google.com/workspace/drive/api/reference/rest/v3/files/create)
