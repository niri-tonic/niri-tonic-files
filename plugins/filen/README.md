# Filen SDK adapter (AGPL-3.0-only)

This separately executed Node.js component uses official `@filen/sdk` **0.4.2**.
The Rust application is GPL-3.0-only; no SDK code is linked into Rust.
Install with `npm ci --ignore-scripts` in this directory. Ship this directory,
including production `node_modules`, under `share/niri-tonic-files/plugins/filen`.
Node.js must be available on PATH. A per-instance `command` argv override is supported.

## Protocol 1

One JSON object per UTF-8 line on stdin/stdout. Requests:
`{"protocol":1,"id":1,"op":"hello","args":{"instance":"personal","read_only":false}}`.
The reply repeats `protocol` and `id`, with `result`, `error`, or `progress`.
Hello returns `{"connector":"personal"}`; login takes SDK `email`, `password`,
`twoFactorCode` through stdin only. Each process owns one SDK/account, and requests
are serialized. Credentials/sessions are not saved to disk. A running process keeps
its authenticated SDK session. After disconnect/cancellation the next operation
starts a new process and resolves credential references again. No mutation is
automatically retried.

Requests have bounded deadlines: hello 30 seconds, login and ordinary operations
120 seconds. Reads/uploads/copies allow five minutes without increasing byte/item
progress and have a 24-hour absolute ceiling. Repeated unchanged progress cannot
extend a request. Pipe writes run separately so a child that stops reading stdin
cannot block cancellation or deadlines. Timeout errors contain no peer text, drop
the session, and never replay the operation. Shutdown gives the process 300 ms to
clean up, then kills and reaps it; ignored cancellation is bounded to two seconds.

Operations: list, stat, read (offset/limit, at most 1 MiB, JSON byte array),
write_new (private local `source` filename), create_directory, rename, copy,
copy_unique, move_item, delete (`mode`: trash/permanent), archive, restore.
All use `path` (percent-encoded absolute file URI); two-path operations also take
`destination`. IDs remain scoped to the configured instance. Cancel uses the
active request ID; shutdown exits and cleans local staging. Progress reports
phase/bytes/items/cancellable. Generic SDK errors are redacted.

## Provider semantics verified against the pinned SDK source

`dist/node/cloud/index.js`: `listDirectory({uuid})` calls API v3 directly and
decrypts fresh directory contents; the vFS cached tree is never used.
Collision preflight additionally checks `fileExists` and `directoryExists`
server-side name hashes, since SDK listings can omit metadata-decryption failures.
`renameFile({uuid,metadata,name,overwriteIfExists:false})` and
`renameDirectory({uuid,name,overwriteIfExists:false})` check existing names.
`moveFile/moveDirectory({uuid,to,metadata,overwriteIfExists:false})` do not
themselves reject every existing name. `createDirectory` may return an existing
UUID, which is verified after creation. `uploadLocalFile({source,parent,name,
abortSignal,onProgress})` streams chunks but has **no exclusive-create flag**.
Therefore uploads use unpredictable `.niri-transfer-<UUID>` names, followed by
fresh collision checks and non-overwriting rename. Moves also use unique
intermediate names when crossing directories. Read uses the UUID-based
`downloadFileToReadableStream` inclusive start/end range; copies stream through
private local files via `downloadFileToLocal` and `uploadLocalFile`.

There is no provider atomic compare-and-create primitive: another client can
race the final check/rename. `atomic_move` is false. Failures/cancellation may
leave completed copies or visible `.niri-transfer-*` items; these are deliberately
retained rather than deleting potentially valuable data. Refresh before retrying.
Recursive copy is incremental, not transactional. Archive is an ordinary sibling
`.archive` directory, and restore moves back to its parent (not version history
or recovery from provider Trash).

## Tests and dependencies

`npm test` injects an in-memory SDK; no account/network calls or real credentials.
Rust connector tests inject a local RPC executable. Live authentication, encrypted
service transfers and external-client races remain unverified.

Scoped overrides in `package.json` select Axios 0.34.0 and elliptic 6.6.1 because
the SDK's dependency ranges retain older vulnerable versions. Compatibility tests
cover the SDK's Axios interface, EC key conversion and RSA DER/PEM conversion.
The adapter uses the SDK's native Node HTTP transport and RSA account keys.

elliptic 6.6.1 is affected by
[GHSA-848j-6mx2-7j84](https://github.com/advisories/GHSA-848j-6mx2-7j84).
Run `npm audit` for current findings; reassess the overrides when updating the SDK.
