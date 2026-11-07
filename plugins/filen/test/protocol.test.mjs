// SPDX-License-Identifier: AGPL-3.0-only
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { fileURLToPath } from 'node:url';

function peer(t) {
  const child = spawn(process.execPath, [fileURLToPath(new URL('../adapter.mjs', import.meta.url))], { stdio: ['pipe', 'pipe', 'pipe'] });
  t.after(() => child.kill());
  let errors = ''; child.stderr.on('data', data => { errors += data; });
  return { child, errors: () => errors };
}
test('real adapter handshake requires no authentication; EOF terminates cleanly', { timeout: 5000 }, async t => {
  const { child, errors } = peer(t);
  const response = once(child.stdout, 'data');
  child.stdin.write(JSON.stringify({ protocol: 1, id: 1, op: 'hello', args: { instance: 'offline-fixture', read_only: true } }) + '\n');
  const [data] = await response;
  assert.deepEqual(JSON.parse(data), { protocol: 1, id: 1, result: { connector: 'offline-fixture' } });
  const exited = once(child, 'exit'); child.stdin.end();
  assert.equal((await exited)[0], 0); assert.equal(errors(), '');
});
test('malformed/version-mismatched frames terminate without echoing input', { timeout: 5000 }, async t => {
  for (const frame of ['SYNTHETIC_SECRET_INVALID_JSON\n', '{"protocol":99,"id":1,"op":"hello"}\n']) {
    const { child, errors } = peer(t); let output = '';
    child.stdout.on('data', data => { output += data; });
    const exited = once(child, 'exit'); child.stdin.write(frame);
    assert.equal((await exited)[0], 2); assert.equal(output, ''); assert.equal(errors(), '');
  }
});
