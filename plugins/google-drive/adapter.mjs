// SPDX-License-Identifier: GPL-3.0-only
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { DriveAdapter } from './core.mjs';
import { AdapterError, IO } from './common.mjs';
import { withProgress } from './progress.mjs';

process.umask(0o077);
const MAX_INPUT = 2 * 1024 * 1024, MAX_OUTPUT = 5 * 1024 * 1024;
let adapter, temporary, active, closing = false, buffer = Buffer.alloc(0);
async function send(frame) {
  if (closing) return;
  const line = JSON.stringify({ protocol: 1, ...frame }) + '\n';
  if (Buffer.byteLength(line) > MAX_OUTPUT) throw new AdapterError('unsupported', 'Result exceeds the protocol frame limit.');
  await new Promise((resolve, reject) => process.stdout.write(line, error => error ? reject(error) : resolve()));
}
async function cleanup(code = 0) {
  if (closing) return; closing = true;
  active?.controller.abort();
  await active?.done;
  if (temporary) await rm(temporary, { force: true, recursive: true });
  process.exit(code);
}
process.on('SIGTERM', () => cleanup()); process.on('SIGINT', () => cleanup());
process.stdin.on('end', () => cleanup()); process.stdin.on('error', () => cleanup(2));
process.stdout.on('error', () => cleanup(2));
async function request(frame, controller) {
  try {
    let result;
    if (frame.op === 'hello') {
      if (adapter || typeof frame.args?.instance !== 'string' || !frame.args.instance || frame.args.instance === 'local' || typeof frame.args.read_only !== 'boolean')
        throw new AdapterError('invalid_input', 'Invalid hello.');
      temporary = await mkdtemp(path.join(tmpdir(), 'niri-google-drive-'));
      adapter = new DriveAdapter(frame.args.instance, frame.args.read_only, temporary);
      result = { connector: frame.args.instance };
    } else {
      if (!adapter) throw new AdapterError('invalid_input', 'Hello is required.');
      if (frame.op === 'login') { await adapter.login(frame.args, controller.signal); result = null; }
      else {
        result = await withProgress(frame.id, frame.op, send,
          progress => adapter.run(frame.op, frame.args, controller.signal, progress),
          { onError: () => controller.abort() });
      }
    }
    await send({ id: frame.id, result });
  } catch (error) {
    await send({ id: frame.id, error: { code: controller.signal.aborted ? 'cancelled' : error instanceof AdapterError ? error.code : 'io',
      message: controller.signal.aborted ? 'Cancelled. Completed items have been kept.' : error instanceof AdapterError ? error.message : IO, retryable: false } });
  }
}
function accept(frame) {
  if (!frame || frame.protocol !== 1 || !Number.isSafeInteger(frame.id) || frame.id < 0 || typeof frame.op !== 'string') throw new Error();
  if (frame.op === 'cancel') { if (active?.id === frame.id) active.controller.abort(); return; }
  if (frame.op === 'shutdown') { void cleanup(); return; }
  if (active || !frame.args || typeof frame.args !== 'object' || Array.isArray(frame.args)) throw new Error();
  const controller = new AbortController();
  const job = { id: frame.id, controller }; active = job;
  job.done = request(frame, controller).catch(() => { void cleanup(2); }).finally(() => { if (active === job) active = undefined; });
}
process.stdin.on('data', chunk => {
  if (closing) return;
  buffer = Buffer.concat([buffer, chunk]);
  let newline;
  while ((newline = buffer.indexOf(10)) >= 0) {
    if (newline > MAX_INPUT) { void cleanup(2); return; }
    const line = buffer.subarray(0, newline); buffer = buffer.subarray(newline + 1);
    try { accept(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(line))); }
    catch { void cleanup(2); return; }
  }
  if (buffer.length > MAX_INPUT) void cleanup(2);
});
