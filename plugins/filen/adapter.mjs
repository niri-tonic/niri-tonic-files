// SPDX-License-Identifier: AGPL-3.0-only
import { FilenSDK } from '@filen/sdk';
import { FilenAdapter, AdapterError } from './core.mjs';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

process.umask(0o077);
// SDK diagnostics must never share the protocol or disclose credentials.
console.log = console.info = console.warn = console.error = console.debug = () => {};
const send = frame => process.stdout.write(JSON.stringify({ protocol: 1, ...frame }) + '\n');
let adapter, tempRoot, active, closed = false;
async function cleanup() {
  if (closed) return; closed = true; active?.controller.abort();
  if (tempRoot) await rm(tempRoot, { recursive: true, force: true });
  process.exit(0);
}
process.on('SIGTERM', cleanup); process.on('SIGINT', cleanup);
process.stdin.on('end', cleanup);
async function request(frame) {
  if (frame.protocol !== 1 || !Number.isSafeInteger(frame.id) || typeof frame.op !== 'string') throw new Error('frame');
  if (frame.op === 'cancel') { if (active?.id === frame.id) active.controller.abort(); return; }
  if (frame.op === 'shutdown') { await cleanup(); return; }
  if (active) throw new Error('concurrent');
  const controller = new AbortController(); active = { id: frame.id, controller };
  try {
    let result;
    if (frame.op === 'hello') {
      if (adapter || typeof frame.args.instance !== 'string' || !frame.args.instance || frame.args.instance === 'local') throw new Error('hello');
      tempRoot = await mkdtemp(path.join(tmpdir(), 'niri-filen-'));
      const sdk = new FilenSDK({ metadataCache: false, connectToSocket: false, tmpPath: tempRoot });
      adapter = new FilenAdapter(sdk, frame.args.instance, Boolean(frame.args.read_only), tempRoot);
      result = { connector: frame.args.instance };
    } else if (frame.op === 'login') {
      if (!adapter) throw new Error('hello required');
      try { await adapter.sdk.login(frame.args); } catch { throw new AdapterError('authentication_required', 'Filen authentication failed. Check your credential references.'); }
      result = null;
    } else {
      if (!adapter) throw new Error('hello required');
      let bytes = 0;
      result = await adapter.run(frame.op, frame.args, controller.signal, delta => {
        if (typeof delta === 'number' && Number.isFinite(delta) && delta >= 0) bytes += delta;
        send({ id: frame.id, progress: { phase: frame.op, bytes: Math.floor(bytes), items: 0, cancellable: true } });
      });
    }
    send({ id: frame.id, result });
  } catch (error) {
    const cancelled = controller.signal.aborted;
    send({ id: frame.id, error: { code: cancelled ? 'cancelled' : error instanceof AdapterError ? error.code : 'io',
      message: cancelled ? 'Cancelled. Completed items have been kept.' : error instanceof AdapterError ? error.message : 'Filen operation failed. Refresh before retrying; completed items or .niri-transfer files may remain.', retryable: false } });
  } finally { active = undefined; }
}
let buffer = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => {
  buffer += chunk;
  if (buffer.length > 2 * 1024 * 1024) { process.exit(2); }
  let newline;
  while ((newline = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, newline); buffer = buffer.slice(newline + 1);
    try { const frame = JSON.parse(line); request(frame).catch(() => process.exit(2)); }
    catch { process.exit(2); }
  }
});
