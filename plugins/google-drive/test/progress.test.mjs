// SPDX-License-Identifier: GPL-3.0-only
import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { withProgress } from '../progress.mjs';

test('progress streams before completion, accumulates bytes/items, and excludes work credentials', async () => {
  const frames = []; let completed = false;
  await withProgress(7, 'copy', async frame => {
    frames.push(frame); assert.equal(completed, false);
  }, async progress => {
    await progress({ bytes: 10, items: 1, token: 'secret-token', code: 'secret-code' });
    assert.equal(frames.length, 1);
    await progress(20); await progress({ items: 2 });
    await delay(25);
    assert.ok(frames.length >= 2);
    return 'result';
  }, { interval: 10 });
  completed = true;
  const last = frames.at(-1).progress;
  assert.equal(last.bytes, 30); assert.equal(last.items, 3);
  for (let n = 1; n < frames.length; n++) {
    assert.ok(frames[n].progress.bytes >= frames[n - 1].progress.bytes);
    assert.ok(frames[n].progress.items >= frames[n - 1].progress.items);
  }
  assert.ok(!JSON.stringify(frames).includes('secret'));
});

test('idle API work gets heartbeats; blocked stdout has one bounded write and backpressures work', async () => {
  let release, started, writes = 0, active = 0, maximum = 0, workAdvanced = false;
  const ready = new Promise(resolve => { started = resolve; });
  const blocked = new Promise(resolve => { release = resolve; });
  const running = withProgress(1, 'write_new', async () => {
    writes++; active++; maximum = Math.max(maximum, active); started();
    if (writes === 1) await blocked;
    active--;
  }, async progress => {
    await delay(15); // Heartbeat while no callback has occurred yet.
    await progress(100); workAdvanced = true;
  }, { interval: 5 });
  await ready; await delay(35);
  assert.equal(writes, 1); assert.equal(workAdvanced, false);
  release(); await running;
  assert.equal(maximum, 1); assert.equal(workAdvanced, true);
  const finishedWrites = writes; await delay(15); assert.equal(writes, finishedWrites);
});

test('write failure aborts work and does not leave a heartbeat loop', async () => {
  const controller = new AbortController(); let writes = 0;
  await assert.rejects(withProgress(1, 'copy', async () => { writes++; throw new Error('closed'); },
    async progress => { await progress({ items: 1 }); },
    { interval: 5, onError: () => controller.abort() }), /closed/);
  assert.equal(controller.signal.aborted, true);
  await delay(15); assert.equal(writes, 1);
});
