// SPDX-License-Identifier: GPL-3.0-only
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createInterface } from 'node:readline';

function launch(t) {
  const child = spawn(process.execPath, [new URL('../adapter.mjs', import.meta.url).pathname], { stdio: ['pipe', 'pipe', 'pipe'] });
  t.after(() => child.kill());
  const lines = createInterface({ input: child.stdout });
  let stderr = ''; child.stderr.on('data', chunk => { stderr += chunk; });
  child.stdin.on('error', () => {});
  return { child, lines, stderr: () => stderr };
}
async function rpc(s, id, op, args) {
  const result = once(s.lines, 'line');
  s.child.stdin.write(JSON.stringify({ protocol: 1, id, op, args }) + '\n');
  return JSON.parse((await result)[0]);
}
test('RPC hello and sanitized errors; clean EOF and stdout contains only frames', async t => {
  const s = launch(t);
  assert.deepEqual(await rpc(s, 1, 'hello', { instance: 'one', read_only: false }), { protocol: 1, id: 1, result: { connector: 'one' } });
  const error = await rpc(s, 2, 'secret-code-do-not-echo', { path: 'file:///' });
  assert.equal(error.error.code, 'unsupported'); assert.ok(!JSON.stringify(error).includes('secret-code'));
  const done = once(s.child, 'exit'); s.child.stdin.end(); assert.equal((await done)[0], 0); assert.equal(s.stderr(), '');
});
test('shutdown succeeds after hello without accessing an account', async t => {
  const s = launch(t); await rpc(s, 1, 'hello', { instance: 'one', read_only: true });
  const done = once(s.child, 'exit'); s.child.stdin.write(JSON.stringify({ protocol: 1, id: 2, op: 'shutdown' }) + '\n');
  assert.equal((await done)[0], 0); assert.equal(s.stderr(), '');
});
test('invalid UTF-8, oversized and concurrent frames terminate safely', async t => {
  for (const data of [Buffer.from([0xff, 10]), Buffer.alloc(2 * 1024 * 1024 + 1, 120),
    Buffer.from([1, 2].map(id => JSON.stringify({ protocol: 1, id, op: 'hello', args: { instance: 'one', read_only: false } })).join('\n') + '\n')]) {
    const s = launch(t), done = once(s.child, 'exit'); s.child.stdin.write(data);
    assert.equal((await done)[0], 2); assert.equal(s.stderr(), '');
  }
});
