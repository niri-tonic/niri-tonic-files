// SPDX-License-Identifier: GPL-3.0-only
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { DriveAdapter, FOLDER, virtualNames, encodeName, transferName } from '../core.mjs';
import { uri } from '../common.mjs';

const caps = { canEdit: true, canAddChildren: true, canDownload: true, canCopy: true, canRename: true, canMoveItemWithinDrive: true, canTrash: true, canDelete: true };
const file = (id, name, extra = {}) => ({ id, name, mimeType: 'text/plain', size: '5', version: '1', modifiedTime: '2026-01-01T00:00:00Z', parents: ['root'], capabilities: { ...caps }, ...extra });
const response = (value, status = 200, headers = {}) => new Response(status === 204 ? null : JSON.stringify(value), { status, headers });
async function fixture(t, initial = []) {
  const temp = await mkdtemp(path.join(tmpdir(), 'drive-core-test-')); t.after(() => rm(temp, { recursive: true, force: true }));
  const items = new Map([['root', file('root', 'My Drive', { mimeType: FOLDER, parents: [] })], ...initial.map(i => [i.id, i])]);
  const calls = [], sessions = new Map(); let sequence = 0;
  const state = { items, calls, temp, hook: null, exports: 0 };
  const auth = { accessToken: async () => 'private-access', request: async (input, options, signal) => {
    signal?.throwIfAborted(); const url = new URL(input), method = options.method ?? 'GET';
    calls.push({ url, method, options });
    const override = await state.hook?.(url, options); if (override) return override;
    const id = decodeURIComponent(url.pathname.split('/')[4] ?? ''), item = items.get(id);
    if (url.pathname.startsWith('/upload/')) {
      if (method === 'POST') {
        const sid = String(++sequence); sessions.set(sid, { metadata: JSON.parse(options.body), size: Number(options.headers['X-Upload-Content-Length']), data: [] });
        return new Response(null, { headers: { location: `https://www.googleapis.com/upload/drive/v3/files?upload_id=${sid}` } });
      }
      const sid = url.searchParams.get('upload_id'), session = sessions.get(sid); session.data.push(options.body);
      const length = session.data.reduce((sum, b) => sum + b.length, 0);
      if (length < session.size) return new Response(null, { status: 308, headers: { range: `bytes=0-${length - 1}` } });
      const made = file('upload-' + sid, session.metadata.name, { ...session.metadata, size: String(length) }); items.set(made.id, made); state.uploaded = Buffer.concat(session.data); return response(made);
    }
    if (!id && method === 'GET') {
      const parent = /^'([^']+)'/.exec(url.searchParams.get('q'))[1];
      const list = [...items.values()].filter(i => i.parents.includes(parent) && !i.trashed);
      // Every page contains one item, exercising pagination on every lookup.
      const n = Number(url.searchParams.get('pageToken') ?? 0);
      return response({ files: list.slice(n, n + 1), ...(n + 1 < list.length ? { nextPageToken: String(n + 1) } : {}) });
    }
    if (!id && method === 'POST') { const made = file('new-' + ++sequence, '', JSON.parse(options.body)); items.set(made.id, made); return response(made); }
    if (!item) return response({}, 404);
    if (url.pathname.endsWith('/export')) { state.exports++; return new Response(Buffer.from('office-bytes')); }
    if (url.pathname.endsWith('/copy')) { const made = { ...item, ...JSON.parse(options.body), id: 'copy-' + ++sequence }; items.set(made.id, made); return response(made); }
    if (method === 'PATCH') {
      Object.assign(item, JSON.parse(options.body));
      if (url.searchParams.has('addParents')) item.parents = [url.searchParams.get('addParents')];
      item.version = String(Number(item.version) + 1); return response(item);
    }
    if (method === 'DELETE') { items.delete(id); return response(null, 204); }
    if (url.searchParams.get('alt') === 'media') {
      const [, a, b] = /^bytes=(\d+)-(\d+)$/.exec(options.headers.Range);
      return new Response(Buffer.from('hello').subarray(Number(a), Number(b) + 1), { status: 206, headers: { 'content-range': `bytes ${a}-${b}/${item.size}` } });
    }
    return response(item);
  } };
  state.adapter = new DriveAdapter('account', false, temp, { auth }); return state;
}
const run = (s, op, p, more = {}, signal) => s.adapter.run(op, { path: uri(p), ...more }, signal);

test('ordinary names remain readable; unsafe/reserved names and duplicates cannot alias', async t => {
  const source = [file('a', 'same'), file('b', 'same'), file('c', '~iYQ~same'), file('d', 'a/b'), file('e', '..'), file('f', 'ordinary.txt'), file('g', 'a\\b')];
  const s = await fixture(t, source); const listing = await run(s, 'list', '/');
  assert.equal(listing.entries.length, 7); assert.equal(new Set(listing.entries.map(e => e.name)).size, 7);
  assert.ok(listing.entries.some(e => e.name === 'ordinary.txt'));
  for (const e of listing.entries) {
    assert.equal(path.posix.basename(decodeURIComponent(e.location.id.slice(7))), e.name);
    assert.equal((await run(s, 'stat', '/' + e.name)).revision, e.revision);
  }
  const alias = listing.entries.find(e => e.revision.includes('"a"')).name;
  s.items.delete('b'); assert.ok((await run(s, 'stat', '/' + alias)).revision.includes('"a"'));
  s.items.get('a').parents = ['elsewhere']; await assert.rejects(run(s, 'stat', '/' + alias), { code: 'not_found' });
  assert.notEqual(encodeName('~eYS9i'), encodeName('a/b'));
});

test('native and binary display collisions get ID aliases; exported extension is retained', () => {
  const names = virtualNames([file('a', 'Doc', { mimeType: 'application/vnd.google-apps.document' }), file('b', 'Doc.docx'), file('c', 'a/b', { mimeType: 'application/vnd.google-apps.document' })]);
  assert.ok(names[0].virtual.startsWith('~i')); assert.ok(names[1].virtual.startsWith('~i'));
  assert.ok(names[2].virtual.endsWith('.docx'));
});

test('ID aliases copy to fresh names and IDs; moves retain original item identity', async t => {
  const s = await fixture(t, [file('a', 'same'), file('b', 'same'), file('dir', 'folder', { mimeType: FOLDER })]);
  const alias = virtualNames([s.items.get('a'), s.items.get('b')])[0].virtual;
  await run(s, 'copy', '/' + alias, { destination: uri('/folder/' + alias) });
  const copied = await run(s, 'stat', '/folder/' + transferName(alias));
  assert.ok(!copied.revision.includes('"a"'));
  const unique = await run(s, 'copy_unique', '/' + alias, { destination: uri('/folder/' + alias) });
  assert.equal(unique.id, uri('/folder/' + transferName(alias) + '-1'));
  await run(s, 'move_item', '/' + alias, { destination: uri('/folder/' + alias) });
  assert.ok((await run(s, 'stat', '/folder/' + alias)).revision.includes('"a"'));
});

test('transfer_name is portable, preserves extensions, and cannot select the source ID', async t => {
  const data = [file('a', 'same.txt'), file('b', 'same.txt'), file('c', 'a/b.txt'),
    file('d', '~iYQ~same.txt'), file('e', 'a/b', { mimeType: 'application/vnd.google-apps.document' }),
    file('f', 'normal.txt'), file('g', '\\bad\x00.xlsx')];
  const s = await fixture(t, data), listing = await run(s, 'list', '/');
  for (const entry of listing.entries) {
    const original = data.find(i => entry.revision.includes(JSON.stringify(i.id)));
    if (original.id === 'f') { assert.equal(entry.transfer_name, null); continue; }
    assert.equal(entry.transfer_name, transferName(entry.name, original));
    assert.ok(!/^[~]|[/\\\x00-\x1f\x7f]/u.test(entry.transfer_name));
    assert.ok(!['', '.', '..'].includes(entry.transfer_name));
    assert.ok(entry.transfer_name.endsWith(original.id === 'e' ? '.docx' : original.id === 'g' ? '.xlsx' : '.txt'));
  }
  assert.equal(transferName('~iYQ~same.txt'), 'same~drive-YQ.txt');
  assert.equal(listing.entries.find(e => e.revision.includes('"c"')).transfer_name, 'Drive-Yw.txt');
});

test('recursive copy preserves duplicate files and native types and emits every committed item', async t => {
  const native = 'application/vnd.google-apps.document';
  const s = await fixture(t, [file('dir', 'source', { mimeType: FOLDER }),
    file('a', 'same.txt', { parents: ['dir'] }), file('b', 'same.txt', { parents: ['dir'] }),
    file('c', 'Doc', { parents: ['dir'], mimeType: native }), file('d', 'Doc', { parents: ['dir'], mimeType: native }),
    file('e', 'a/b.txt', { parents: ['dir'] }), file('f', 'same~drive-YQ.txt', { parents: ['dir'] })]);
  const changes = [];
  await s.adapter.run('copy', { path: uri('/source'), destination: uri('/destination') }, undefined, async delta => { changes.push(delta); });
  assert.equal(changes.reduce((sum, d) => sum + (d.items ?? 0), 0), 7);
  const result = (await run(s, 'list', '/destination')).entries;
  assert.equal(result.length, 6); assert.equal(new Set(result.map(e => e.name)).size, 6);
  assert.equal([...s.items.values()].filter(i => i.id.startsWith('copy-') && i.mimeType === native).length, 2);
  assert.equal(s.exports, 0);
  for (const e of result) assert.equal(e.transfer_name, null);
});

test('metadata revisions remain stable across list/stat/read and export cache eviction beyond 16 documents', async t => {
  const s = await fixture(t, Array.from({ length: 18 }, (_, n) => file('doc-' + n, 'Doc' + n, { mimeType: 'application/vnd.google-apps.document', size: undefined })));
  const before = (await run(s, 'list', '/')).entries;
  for (const e of before) {
    const stat = await run(s, 'stat', '/' + e.name);
    assert.equal(stat.revision, e.revision); assert.equal(stat.size, 12);
  }
  assert.equal(s.adapter.exports.size, 16);
  assert.deepEqual((await run(s, 'list', '/')).entries.map(e => e.revision), before.map(e => e.revision));
  const bytes = await run(s, 'read', '/Doc0.docx', { offset: 0, limit: 100 });
  assert.equal(Buffer.from(bytes).toString(), 'office-bytes');
  assert.equal((await run(s, 'stat', '/Doc0.docx')).revision, before[0].revision);
});

test('cursor loops and incomplete listings fail closed', async t => {
  const s = await fixture(t);
  s.hook = url => url.pathname.endsWith('/files') ? response({ files: [], nextPageToken: 'loop' }) : null;
  await assert.rejects(run(s, 'list', '/'), /cursor loop/);
  s.hook = url => url.pathname.endsWith('/files') ? response({ files: [], incompleteSearch: true }) : null;
  await assert.rejects(run(s, 'list', '/'), /incomplete/);
});

test('bounded ordinary read validates ranges, length and source revision', async t => {
  const s = await fixture(t, [file('a', 'hello')]);
  assert.deepEqual(await run(s, 'read', '/hello', { offset: 1, limit: 3 }), [101, 108, 108]);
  await assert.rejects(run(s, 'read', '/hello', { offset: 0, limit: 1048577 }), { code: 'invalid_input' });
  s.hook = url => url.searchParams.get('alt') === 'media' ? new Response('hello') : null;
  await assert.rejects(run(s, 'read', '/hello', { offset: 0, limit: 5 }), /byte range/);
  s.hook = url => {
    if (url.searchParams.get('alt') !== 'media') return null;
    s.items.get('a').version = '2';
    return new Response('hello', { status: 206, headers: { 'content-range': 'bytes 0-4/5' } });
  };
  await assert.rejects(run(s, 'read', '/hello', { offset: 0, limit: 5 }), /Source changed/);
});

test('stat exports exact size with revision cache; reads match; unsupported native and shortcuts fail', async t => {
  const s = await fixture(t, [file('a', 'Doc', { mimeType: 'application/vnd.google-apps.document', size: undefined }), file('b', 'Form', { mimeType: 'application/vnd.google-apps.form' }), file('c', 'Link', { mimeType: 'application/vnd.google-apps.shortcut' })]);
  const stat = await run(s, 'stat', '/Doc.docx'); assert.equal(stat.size, 12);
  assert.equal(Buffer.from(await run(s, 'read', '/Doc.docx', { offset: 0, limit: 20 })).toString(), 'office-bytes');
  assert.equal(s.exports, 1); assert.equal((await run(s, 'stat', '/Doc.docx')).revision, stat.revision);
  s.items.get('a').version = '2'; assert.notEqual((await run(s, 'stat', '/Doc.docx')).revision, stat.revision); assert.equal(s.exports, 2);
  await assert.rejects(run(s, 'stat', '/Form'), { code: 'unsupported' });
  await assert.rejects(run(s, 'stat', '/Link'), { code: 'unsupported' });
});

test('same-account native copies retain native MIME; move/archive/restore/delete use IDs', async t => {
  const s = await fixture(t, [file('a', 'Doc', { mimeType: 'application/vnd.google-apps.document' })]);
  await run(s, 'copy', '/Doc.docx', { destination: uri('/Copy.docx') });
  assert.equal([...s.items.values()].find(i => i.name === 'Copy').mimeType, 'application/vnd.google-apps.document'); assert.equal(s.exports, 0);
  await run(s, 'rename', '/Copy.docx', { destination: uri('/Renamed.docx') });
  const archived = await run(s, 'archive', '/Renamed.docx'); assert.equal(archived.id, uri('/.archive/Renamed.docx'));
  await run(s, 'restore', '/.archive/Renamed.docx');
  await run(s, 'delete', '/Renamed.docx', { mode: 'trash' });
  assert.ok([...s.items.values()].find(i => i.name === 'Renamed').trashed);
  await run(s, 'delete', '/Doc.docx', { mode: 'permanent' }); assert.ok(!s.items.has('a'));
});

test('copy_unique, fresh collisions and cross-client races never overwrite', async t => {
  const s = await fixture(t, [file('a', 'hello'), file('b', 'copy')]);
  const result = await run(s, 'copy_unique', '/hello', { destination: uri('/copy') }); assert.equal(result.id, uri('/copy-1'));
  await assert.rejects(run(s, 'create_directory', '/copy'), { code: 'already_exists' });
  s.hook = (url, options) => {
    if (url.pathname.endsWith('/copy') && options.method === 'POST') s.items.set('racer', file('racer', 'raced'));
  };
  await assert.rejects(run(s, 'copy', '/hello', { destination: uri('/raced') }), { code: 'already_exists' });
  assert.equal([...s.items.values()].filter(i => i.name === 'raced').length, 2);
});

test('uploads chunk private spools and retain staging on ambiguous failure without retry', async t => {
  const s = await fixture(t); const spool = path.join(s.temp, 'spool');
  const data = Buffer.alloc(1048576 + 10, 7); await writeFile(spool, data, { mode: 0o600 });
  await run(s, 'write_new', '/uploaded', { source: spool }); assert.deepEqual(s.uploaded, data);
  assert.equal(s.calls.filter(c => c.method === 'PUT').length, 2);
  assert.ok([...s.items.values()].some(i => i.name === 'uploaded'));
  let failures = 0;
  s.hook = (url, options) => {
    if (url.pathname.startsWith('/upload/') && options.method === 'PUT') { failures++; throw new Error('ambiguous'); }
  };
  await assert.rejects(run(s, 'write_new', '/failed', { source: spool })); assert.equal(failures, 1);
});

test('PDF, Office and image uploads retain media types and never request native conversion', async t => {
  const s = await fixture(t), spool = path.join(s.temp, 'spool');
  await writeFile(spool, 'offline fixture bytes', { mode: 0o600 });
  for (const [name, type] of [['report.pdf', 'application/pdf'],
    ['Doc.docx', 'application/vnd.openxmlformats-officedocument.wordprocessingml.document'],
    ['photo.PNG', 'image/png']]) {
    const start = s.calls.length;
    await run(s, 'write_new', '/' + name, { source: spool });
    const calls = s.calls.slice(start), initiation = calls.find(c => c.method === 'POST' && c.url.pathname.startsWith('/upload/'));
    const metadata = JSON.parse(initiation.options.body);
    assert.equal(Object.hasOwn(metadata, 'mimeType'), false);
    assert.ok(metadata.name.endsWith(path.posix.extname(name)));
    assert.equal(initiation.options.headers['X-Upload-Content-Type'], type);
    assert.equal(calls.find(c => c.method === 'PUT').options.headers['Content-Type'], type);
    assert.ok(!JSON.stringify(metadata).includes('application/vnd.google-apps.'));
  }
});

test('read-only, root mutation, capabilities and cooperative cancellation prevent writes', async t => {
  const s = await fixture(t, [file('a', 'hello', { capabilities: { ...caps, canDelete: false } })]);
  await assert.rejects(run(s, 'delete', '/', { mode: 'permanent' }), { code: 'permission_denied' });
  await assert.rejects(run(s, 'delete', '/hello', { mode: 'permanent' }), { code: 'permission_denied' });
  s.adapter.readOnly = true; await assert.rejects(run(s, 'create_directory', '/dir'), { code: 'permission_denied' });
  const controller = new AbortController(); controller.abort();
  await assert.rejects(run(s, 'list', '/', {}, controller.signal), { code: 'cancelled' });
  assert.equal(s.calls.filter(c => c.method !== 'GET').length, 0);
});

test('oversized export is rejected and private partial cache removed', async t => {
  const s = await fixture(t, [file('a', 'Doc', { mimeType: 'application/vnd.google-apps.document' })]);
  s.hook = url => url.pathname.endsWith('/export') ? new Response(Buffer.alloc(10 * 1024 * 1024 + 1)) : null;
  await assert.rejects(run(s, 'stat', '/Doc.docx'), { code: 'unsupported' }); assert.deepEqual(await readdir(s.temp), []);
});
