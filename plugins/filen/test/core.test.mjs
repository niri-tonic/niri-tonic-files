// SPDX-License-Identifier: AGPL-3.0-only
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { FilenAdapter, remotePath, uri } from '../core.mjs';

function fakeSDK() {
  const items = new Map(), calls = [];
  const cloud = {
    async fileExists({ name, parent }) { return { exists: [...items.values()].some(i => i.type === 'file' && i.name === name && i.parent === parent) }; },
    async directoryExists({ name, parent }) { return { exists: [...items.values()].some(i => i.type === 'directory' && i.name === name && i.parent === parent) }; },
    async listDirectory({ uuid }) { calls.push(['list', uuid]); return [...items.values()].filter(i => i.parent === uuid).map(i => ({ ...i })); },
    async createDirectory({ uuid, name, parent }) { items.set(uuid, { uuid, name, parent, type: 'directory', size: 0 }); return uuid; },
    async renameFile({ uuid, name, overwriteIfExists }) {
      assert.equal(overwriteIfExists, false);
      const item = items.get(uuid);
      assert.ok(![...items.values()].some(i => i.uuid !== uuid && i.parent === item.parent && i.name === name));
      item.name = name;
    },
    async moveFile({ uuid, to, overwriteIfExists }) { assert.equal(overwriteIfExists, false); items.get(uuid).parent = to; },
    async uploadLocalFile({ source, parent, name, abortSignal, onProgress }) {
      assert.equal(abortSignal.aborted, false); assert.match(name, /^\.niri-transfer-/);
      const data = await readFile(source); onProgress(data.length);
      const uuid = 'file-' + items.size; items.set(uuid, { uuid, parent, name, type: 'file', size: data.length, data });
    },
    async downloadFileToLocal({ uuid, to }) { await writeFile(to, items.get(uuid).data); },
    downloadFileToReadableStream({ uuid, start, end }) { return (async function* () { yield items.get(uuid).data.subarray(start, end + 1); })(); },
    async trashFile({ uuid }) { calls.push(['trash', uuid]); items.delete(uuid); },
    async deleteFile({ uuid }) { calls.push(['permanent', uuid]); items.delete(uuid); },
  };
  cloud.renameDirectory = cloud.renameFile; cloud.moveDirectory = cloud.moveFile;
  cloud.trashDirectory = cloud.trashFile; cloud.deleteDirectory = cloud.deleteFile;
  return { config: { baseFolderUUID: 'root' }, cloud: () => cloud, items, calls };
}
async function setup(t, id = 'personal') {
  const temp = await mkdtemp(path.join(tmpdir(), 'filen-isolated-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const sdk = fakeSDK();
  return { sdk, temp, adapter: new FilenAdapter(sdk, id, false, temp), signal: new AbortController().signal };
}
test('strict Unicode URIs and invalid paths', () => {
  assert.equal(remotePath(uri('/é/😀 #?')), '/é/😀 #?');
  for (const value of ['file://host/a', 'file:///a/../b', 'file:///%ff', 'file:///%', 'file:///a//b', 'file:///a%00', 'file:///a/']) {
    assert.throws(() => remotePath(value), { code: 'invalid_input' });
  }
});
test('revisions distinguish same-size same-time replacements and file versions', async t => {
  const { adapter } = await setup(t);
  const item = { uuid: 'original', type: 'file', name: 'file', size: 7, lastModified: 1000, version: 2, hash: 'content-a' };
  const original = adapter.entry(item, '/file');
  assert.notEqual(original.revision, adapter.entry({ ...item, uuid: 'replacement' }, '/file').revision);
  assert.notEqual(original.revision, adapter.entry({ ...item, version: 3 }, '/file').revision);
  assert.notEqual(original.revision, adapter.entry({ ...item, hash: 'content-b' }, '/file').revision);
  assert.equal(original.revision, adapter.entry({ ...item }, '/file').revision);
  assert.equal(adapter.entry({ uuid: 'root', type: 'directory', name: '/' }, '/').revision, null);
});
test('independent instances, fresh listing, all ordinary operations and archive inverse', async t => {
  const { adapter: a, sdk, temp, signal } = await setup(t);
  const { adapter: b } = await setup(t, 'work');
  const run = (op, args) => a.run(op, args, signal);
  await run('create_directory', { path: 'file:///docs' });
  const source = path.join(temp, 'upload'); await writeFile(source, 'hello world');
  await run('write_new', { path: 'file:///docs/a', source });
  assert.equal((await run('stat', { path: 'file:///docs/a' })).size, 11);
  assert.equal(Buffer.from(await run('read', { path: 'file:///docs/a', offset: 6, limit: 5 })).toString(), 'world');
  assert.equal((await b.run('list', { path: 'file:///' }, signal)).entries.length, 0);
  assert.equal((await run('list', { path: 'file:///docs' })).entries[0].location.connector, 'personal');
  await run('copy', { path: 'file:///docs/a', destination: 'file:///docs/b' });
  const unique = await run('copy_unique', { path: 'file:///docs/a', destination: 'file:///docs/b' });
  assert.equal(unique.id, 'file:///docs/b-1');
  await run('rename', { path: unique.id, destination: 'file:///docs/c' });
  const archived = await run('archive', { path: 'file:///docs/c' });
  assert.equal(archived.id, 'file:///docs/.archive/c');
  assert.equal((await run('restore', { path: archived.id })).id, 'file:///docs/c');
  await run('move_item', { path: 'file:///docs/c', destination: 'file:///moved' });
  await run('copy', { path: 'file:///docs', destination: 'file:///docs-copy' });
  await run('delete', { path: 'file:///moved', mode: 'trash' });
  await run('delete', { path: 'file:///docs/b', mode: 'permanent' });
  assert.ok(sdk.calls.some(c => c[0] === 'trash')); assert.ok(sdk.calls.some(c => c[0] === 'permanent'));
  // Simulate an external client: no vFS cache should hide the new entry.
  sdk.items.set('external', { uuid: 'external', parent: 'root', name: 'external', type: 'file', size: 1 });
  assert.ok((await run('list', { path: 'file:///' })).entries.some(i => i.name === 'external'));
});
test('collision preflight preserves data and mutation is denied for root/read-only', async t => {
  const { adapter, sdk, temp, signal } = await setup(t);
  sdk.items.set('existing', { uuid: 'existing', parent: 'root', name: 'Existing', type: 'file', size: 4, data: Buffer.from('keep') });
  const source = path.join(temp, 'source'); await writeFile(source, 'replacement');
  for (const op of ['write_new', 'create_directory']) {
    await assert.rejects(adapter.run(op, { path: 'file:///existing', source }, signal), { code: 'already_exists' });
  }
  assert.equal(sdk.items.get('existing').data.toString(), 'keep');
  await assert.rejects(adapter.run('delete', { path: 'file:///', mode: 'permanent' }, signal), { code: 'permission_denied' });
  adapter.readOnly = true;
  await assert.rejects(adapter.run('write_new', { path: 'file:///new', source }, signal), { code: 'permission_denied' });
});
test('uploads retain MIME extension and copies use numbered names before it', async t => {
  const { adapter, sdk, temp, signal } = await setup(t);
  const upload = sdk.cloud().uploadLocalFile;
  sdk.cloud().uploadLocalFile = async args => {
    assert.equal(path.posix.extname(args.name), '.txt');
    return upload(args);
  };
  const source = path.join(temp, 'unnamed-spool');
  await writeFile(source, 'keep');
  await adapter.run('write_new', { path: 'file:///report.txt', source }, signal);
  for (let n = 1; n <= 2; n++) {
    const copied = await adapter.run('copy_unique', { path: 'file:///report.txt', destination: 'file:///report.txt' }, signal);
    assert.equal(copied.id, `file:///report-${n}.txt`);
  }
  const archived = await adapter.run('archive', { path: 'file:///report.txt' }, signal);
  await assert.rejects(adapter.run('archive', { path: archived.id }, signal), { code: 'invalid_input' });
});
test('cancellation, bounded reads, malformed names and duplicate names', async t => {
  const { adapter, sdk } = await setup(t);
  const c = new AbortController(); c.abort();
  await assert.rejects(adapter.run('create_directory', { path: 'file:///new' }, c.signal), { code: 'cancelled' });
  assert.equal(sdk.items.size, 0);
  await assert.rejects(adapter.run('read', { path: 'file:///x', offset: 0, limit: 1048577 }), { code: 'invalid_input' });
  sdk.items.set('bad', { uuid: 'bad', name: '\ud800', parent: 'root', type: 'file' });
  await assert.rejects(adapter.run('list', { path: 'file:///' }), { code: 'invalid_input' });
  sdk.items.get('bad').name = 'same'; sdk.items.set('other', { uuid: 'other', name: 'same', parent: 'root', type: 'file' });
  await assert.rejects(adapter.run('list', { path: 'file:///' }), { code: 'invalid_input' });
});
test('concurrent collision at upload publication retains both files', async t => {
  const { adapter, sdk, temp, signal } = await setup(t);
  const original = sdk.cloud().uploadLocalFile;
  sdk.cloud().uploadLocalFile = async args => {
    await original(args);
    sdk.items.set('racer', { uuid: 'racer', parent: 'root', name: 'final', type: 'file', data: Buffer.from('racer') });
  };
  const source = path.join(temp, 'source'); await writeFile(source, 'ours');
  await assert.rejects(adapter.run('write_new', { path: 'file:///final', source }, signal), { code: 'already_exists' });
  assert.equal(sdk.items.get('racer').data.toString(), 'racer');
  assert.ok([...sdk.items.values()].some(i => i.name.startsWith('.niri-transfer-') && i.data.toString() === 'ours'));
});
