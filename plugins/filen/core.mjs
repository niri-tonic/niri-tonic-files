// SPDX-License-Identifier: AGPL-3.0-only
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import path from 'node:path';

export class AdapterError extends Error {
  constructor(code, message) { super(message); this.code = code; }
}
const fail = (code, message) => { throw new AdapterError(code, message); };
export function remotePath(value) {
  if (typeof value !== 'string' || !value.startsWith('file:///')) fail('invalid_input', 'Expected an absolute file URI.');
  if (/[?#]/u.test(value)) fail('invalid_input', 'URI query and fragment characters must be percent-encoded.');
  let decoded;
  try { decoded = decodeURIComponent(value.slice(7)); } catch { fail('invalid_input', 'Invalid UTF-8 file URI.'); }
  if (decoded !== '/' && (decoded.endsWith('/') || decoded.split('/').slice(1).some(p => !validName(p)))) fail('invalid_input', 'Invalid remote path.');
  return decoded;
}
export function validName(name) {
  return typeof name === 'string' && name.length > 0 && name !== '.' && name !== '..' &&
    !/[\x00-\x1f\x7f/\\]/u.test(name) && !/[\uD800-\uDFFF]/u.test(name) && !name.startsWith('CANNOT_DECRYPT_');
}
export const uri = p => 'file://' + p.split('/').map(encodeURIComponent).join('/');
const child = (parent, name) => parent === '/' ? '/' + name : parent + '/' + name;

// Every lookup traverses fresh cloud.listDirectory results by UUID. The SDK vFS
// tree is deliberately never used: its cache is not an exclusive-create check.
export class FilenAdapter {
  constructor(sdk, instance, readOnly, tempRoot) {
    this.sdk = sdk; this.cloud = sdk.cloud(); this.instance = instance;
    this.readOnly = readOnly; this.tempRoot = tempRoot;
  }
  check(signal) { if (signal?.aborted) fail('cancelled', 'Cancelled. Completed items have been kept.'); }
  async children(item) {
    if (item.type !== 'directory') fail('invalid_input', 'Expected a directory.');
    const items = await this.cloud.listDirectory({ uuid: item.uuid });
    const names = new Set();
    for (const i of items) {
      if (!validName(i.name)) fail('invalid_input', 'A remote item has an invalid or undecipherable name.');
      if (names.has(i.name)) fail('invalid_input', 'Ambiguous duplicate remote names.');
      names.add(i.name);
    }
    return items;
  }
  async lookup(p) {
    let item = { uuid: this.sdk.config.baseFolderUUID, type: 'directory', name: '/', size: 0 };
    if (!item.uuid) fail('authentication_required', 'Filen authentication is required.');
    for (const name of p.split('/').filter(Boolean)) {
      item = (await this.children(item)).find(i => i.name === name);
      if (!item) fail('not_found', 'Remote item not found.');
    }
    return item;
  }
  entry(item, p) {
    return { location: { connector: this.instance, id: uri(p) }, name: item.name,
      kind: item.type, navigable: item.type === 'directory', hidden: item.name.startsWith('.'),
      size: item.size ?? 0, modified: item.lastModified ? Math.floor(item.lastModified / 1000) : null,
      // SDK `version` is the encryption format, not a content generation counter.
      // Include UUID, content hash and full-resolution metadata when available.
      revision: p === '/' ? null : JSON.stringify([item.uuid, item.version ?? null, item.hash ?? null, item.lastModified ?? null]),
      read_only: this.readOnly };
  }
  mutable(p) {
    if (this.readOnly) fail('permission_denied', 'This Filen account is read-only.');
    if (p === '/') fail('permission_denied', 'The account root cannot be modified.');
  }
  async vacant(p) {
    this.mutable(p);
    const parent = await this.lookup(path.posix.dirname(p));
    const name = path.posix.basename(p);
    // Check all item kinds, including case/whitespace aliases recognized by Filen.
    if ((await this.children(parent)).some(i => i.name.toLowerCase().trim() === name.toLowerCase().trim()))
      fail('already_exists', 'A remote item already exists at the destination.');
    // SDK listDirectory can omit metadata-decryption failures. Also query both
    // server-side name hashes so such entries cannot become overwrite targets.
    const present = await Promise.all([
      this.cloud.fileExists({ name, parent: parent.uuid }),
      this.cloud.directoryExists({ name, parent: parent.uuid }),
    ]);
    if (present.some(result => result.exists)) fail('already_exists', 'A remote item already exists at the destination.');
    return parent;
  }
  async mkdir(p, signal) {
    const parent = await this.vacant(p); this.check(signal);
    const uuid = randomUUID();
    const result = await this.cloud.createDirectory({ uuid, name: path.posix.basename(p), parent: parent.uuid, renameIfExists: false });
    // SDK may return an existing directory on a concurrent collision.
    const made = await this.lookup(p);
    if (made.uuid !== uuid) fail('already_exists', 'The destination appeared during directory creation.');
    return result;
  }
  async renameItem(item, name) {
    const args = { uuid: item.uuid, metadata: item, name, overwriteIfExists: false };
    await this.cloud[item.type === 'file' ? 'renameFile' : 'renameDirectory'](args);
  }
  async move(src, dst, signal) {
    this.mutable(src); await this.vacant(dst);
    if (dst.startsWith(src + '/')) fail('invalid_input', 'Cannot move an item into itself.');
    const item = await this.lookup(src); this.check(signal);
    const sourceParent = path.posix.dirname(src), destParent = path.posix.dirname(dst);
    if (sourceParent === destParent) {
      await this.renameItem(item, path.posix.basename(dst));
    } else {
      const parent = await this.lookup(destParent);
      // Unique intermediate name avoids a collision with the source basename in
      // the destination. Failures retain data under this visible temporary name.
      const intermediate = `.niri-transfer-${randomUUID()}`;
      await this.vacant(child(sourceParent, intermediate));
      await this.renameItem(item, intermediate);
      if ((await this.lookup(child(sourceParent, intermediate))).uuid !== item.uuid)
        fail('io', 'Intermediate remote rename could not be verified. Refresh both directories.');
      item.name = intermediate;
      await this.vacant(child(destParent, intermediate));
      await this.cloud[item.type === 'file' ? 'moveFile' : 'moveDirectory']({ uuid: item.uuid, to: parent.uuid, metadata: item, overwriteIfExists: false });
      if ((await this.lookup(child(destParent, intermediate))).uuid !== item.uuid)
        fail('io', 'Intermediate remote move could not be verified. Refresh both directories.');
      await this.vacant(dst);
      await this.renameItem(item, path.posix.basename(dst));
    }
    if ((await this.lookup(dst)).uuid !== item.uuid) fail('io', 'Remote move could not be verified. Refresh both directories.');
  }
  async upload(dst, source, signal, progress) {
    const parent = await this.vacant(dst); this.check(signal);
    // uploadLocalFile has no exclusive option. Never upload to the final name:
    // upload an unpredictable staging name, then use non-overwriting rename.
    // The SDK derives MIME from this name, so preserve the final extension.
    const name = `.niri-transfer-${randomUUID()}${path.posix.extname(dst)}`;
    const staged = child(path.posix.dirname(dst), name);
    await this.vacant(staged);
    await this.cloud.uploadLocalFile({ source, parent: parent.uuid, name, abortSignal: signal, onProgress: progress });
    this.check(signal); await this.vacant(dst);
    const item = await this.lookup(staged);
    await this.renameItem(item, path.posix.basename(dst));
    if ((await this.lookup(dst)).uuid !== item.uuid) fail('io', 'Remote upload could not be verified. Refresh the directory.');
  }
  async copy(src, dst, signal, progress) {
    await this.vacant(dst);
    if (dst.startsWith(src + '/')) fail('invalid_input', 'Cannot copy an item into itself.');
    const item = await this.lookup(src); this.check(signal);
    if (item.type === 'directory') {
      const items = await this.children(item);
      await this.mkdir(dst, signal);
      for (const i of items) { this.check(signal); await this.copy(child(src, i.name), child(dst, i.name), signal, progress); }
    } else {
      const dir = await mkdtemp(path.join(this.tempRoot, 'copy-'));
      try {
        const to = path.join(dir, 'data');
        await this.cloud.downloadFileToLocal({ ...item, to, abortSignal: signal, onProgress: progress });
        await this.upload(dst, to, signal, progress);
      } finally { await rm(dir, { recursive: true, force: true }); }
    }
  }
  async run(op, args, signal, progress = () => {}) {
    this.check(signal);
    const p = remotePath(args.path);
    switch (op) {
      case 'list': return { entries: (await this.children(await this.lookup(p))).map(i => this.entry(i, child(p, i.name))), next: null };
      case 'stat': return this.entry(await this.lookup(p), p);
      case 'read': {
        if (!Number.isSafeInteger(args.offset) || args.offset < 0 || !Number.isInteger(args.limit) || args.limit < 0 || args.limit > 1048576) fail('invalid_input', 'Invalid bounded read.');
        const item = await this.lookup(p);
        if (item.type !== 'file') fail('invalid_input', 'Only files can be read.');
        if (!args.limit || args.offset >= item.size) return [];
        const end = Math.min(item.size, args.offset + args.limit) - 1;
        const stream = this.cloud.downloadFileToReadableStream({ ...item, start: args.offset, end, abortSignal: signal, onProgress: progress });
        const chunks = []; let size = 0;
        for await (const data of stream) {
          this.check(signal); size += data.length;
          if (size > args.limit) fail('io', 'SDK exceeded the bounded read limit.');
          chunks.push(Buffer.from(data));
        }
        return [...Buffer.concat(chunks)];
      }
      case 'write_new': await this.upload(p, args.source, signal, progress); return null;
      case 'create_directory': await this.mkdir(p, signal); return null;
      case 'rename': case 'move_item': await this.move(p, remotePath(args.destination), signal); return null;
      case 'copy': await this.copy(p, remotePath(args.destination), signal, progress); return null;
      case 'copy_unique': {
        const dst = remotePath(args.destination); let candidate = dst;
        const source = await this.lookup(p);
        const parsed = path.posix.parse(dst);
        for (let n = 1; ; n++) {
          this.check(signal);
          try { await this.vacant(candidate); break; } catch (e) { if (e.code !== 'already_exists') throw e; }
          candidate = path.posix.join(parsed.dir, source.type === 'directory'
            ? `${parsed.base}-${n}` : `${parsed.name}-${n}${parsed.ext}`);
        }
        await this.copy(p, candidate, signal, progress);
        return { connector: this.instance, id: uri(candidate) };
      }
      case 'delete': {
        this.mutable(p); const item = await this.lookup(p); this.check(signal);
        if (!['trash', 'permanent'].includes(args.mode)) fail('invalid_input', 'Invalid delete mode.');
        const method = (args.mode === 'trash' ? 'trash' : 'delete') + (item.type === 'directory' ? 'Directory' : 'File');
        await this.cloud[method]({ uuid: item.uuid }); return null;
      }
      case 'archive': {
        this.mutable(p);
        if (path.posix.basename(p) === '.archive' || path.posix.basename(path.posix.dirname(p)) === '.archive')
          fail('invalid_input', 'This item is an archive or already inside one.');
        const dir = child(path.posix.dirname(p), '.archive');
        try { await this.lookup(dir); } catch (e) { if (e.code !== 'not_found') throw e; await this.mkdir(dir, signal); }
        const dst = child(dir, path.posix.basename(p)); await this.move(p, dst, signal);
        return { connector: this.instance, id: uri(dst) };
      }
      case 'restore': {
        if (path.posix.basename(path.posix.dirname(p)) !== '.archive') fail('invalid_input', 'This item is not in a sibling .archive directory.');
        const dst = child(path.posix.dirname(path.posix.dirname(p)), path.posix.basename(p));
        await this.move(p, dst, signal); return { connector: this.instance, id: uri(dst) };
      }
      default: fail('unsupported', 'Unknown Filen operation.');
    }
  }
}
