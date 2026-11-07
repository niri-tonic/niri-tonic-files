# Connectors and plugin evolution

## In-process contract

`src/connector.rs` defines `Connector: Send + Sync`, `Location`, metadata entries,
capabilities, pagination, authentication states, typed errors, progress and a
cooperative cancellation context. Methods are synchronous **worker-only** APIs:
list/stat, bounded read, streamed exclusive write, create-directory, rename, copy,
move, Trash/permanent-delete, archive and restore. Unsupported actions return
`Unsupported`; read-only connectors must reject mutation themselves as well as
advertise disabled UI actions.

IDs are opaque and scoped by connector. A remote ID need not be a POSIX path.
Capabilities are independent: Trash is not permanent deletion, moving is not
necessarily renaming, and provider-native archiving is not assumed. Pagination
uses an opaque cursor; a provider may reject stale cursors. Totals may be unknown,
so progress is phase/bytes/items plus a `cancellable` flag. Errors identify not-found,
permissions, collision, cancellation, unsupported operation, authentication,
stale cursor and general I/O, with a retry hint.

Local, Filen and Google Drive are implemented. `Registry` routes by unique instance
ID, so multiple accounts of one provider remain independent. Adapters return a
complete listing (Drive consumes API pages internally). Local uses inotify;
remote views use periodic listing and explicit post-operation refresh.

`locations` bridges the existing path-based pane model to scoped locations. Its
remote paths are virtual application paths, never filesystem mounts. The UI,
clipboard, favorites and session files expose `filen://instance/path` or
`drive://instance/path` URIs, selected using the configured instance type. The
registry rejects unknown IDs and prevents a provider from returning entries from
another account. Optional metadata revision tokens strengthen transfer consistency
checks beyond timestamps and size.
An optional `Entry.transfer_name` supplies a validated portable basename for a new
copy when a provider path contains an identity alias (Drive duplicate names). This
keeps source identity separate from destination naming during recursive transfers.

Local additionally implements `copy_unique`, returning the final location after
atomically choosing a numbered destination on collision. The default trait method
retains strict exclusive-copy semantics for providers that do not support automatic
naming. Remote collision policies must be negotiated explicitly.

## External plugin implementation

Filen uses **versioned JSON-lines RPC over child-process stdio**, with an isolated
Node.js SDK session per instance. It does not expose a native Rust dynamic-library
ABI. This avoids compiler/allocator coupling and allows the official TypeScript
SDK to handle Filen authentication and encryption in its separate AGPL component.
Google Drive uses the same `RemoteConnector` Rust host and protocol, with a
separate GPL Node adapter using built-in HTTP/OAuth support and no npm dependencies.
The `filen::Filen` Rust name is retained as a compatibility reexport.

Protocol 1 is documented in [the adapter](../plugins/filen/README.md). It uses
`protocol`, `id`, `op`, `args` request fields; responses contain `result`, `error`
or `progress`. This is a small application protocol, not JSON-RPC 2.0. Example:

```json
{"protocol":1,"id":1,"op":"hello","args":{"instance":"personal","read_only":false}}
```

The host validates protocol/instance/request IDs, bounds response frames to 5 MiB,
limits individual reads to 1 MiB, redacts errors and resolves credentials only on
worker threads. Transfers use private temporary files rather than embedding whole
files in JSON. Deadlines cover handshake, login, requests and stalled transfers.
Cancellation sends a request and, if necessary, terminates/reaps the worker. No
mutation is automatically replayed after a disconnect or timeout.

Cross-connector transfers are coordinated by Rust: stream to private staging,
write the destination, and only then remove the source for a move, after checking
the source tree again. Opening a remote file materializes a private account-scoped
local copy. Copies within one instance delegate to its backend, preserving native
Google document types; copies across providers export those documents. There is no
implicit upload on editor close. See [Filen setup](filen.md) and
[Google Drive setup](google-drive.md) for configuration, collision limits and
validation boundaries. Drive OAuth uses PKCE and a loopback callback, with optional
per-instance owner-only refresh-token files outside YAML. Interactive authorization
has a separate longer deadline from ordinary API operations.

## Feasibility assessment (2026-11-06)

| Provider | Evidence and constraints | Decision |
| --- | --- | --- |
| Filen | Official [TypeScript SDK](https://github.com/FilenCloudDienste/filen-sdk-ts), pinned to 0.4.2, with isolated instance processes. API signatures and non-overwrite behavior were checked against its source. | Implemented; isolated tests only. Real-account auth/encryption/service behavior has not been validated. |
| Google Drive | Official [Drive API](https://developers.google.com/workspace/drive/api/guides/about-sdk), browser OAuth, stable IDs behind virtual names, paginated listings and native document exports. Drive cannot atomically guarantee unique names. | Implemented for My Drive; isolated validation only. Shared drives excluded. |

**Remote exploration and automatic synchronization are different products.** The
contract supports user-initiated operations. It does not include a reconciliation
engine, conflict database, background replication or offline automatic sync.
