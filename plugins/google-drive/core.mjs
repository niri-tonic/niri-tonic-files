// SPDX-License-Identifier: GPL-3.0-only
import path from 'node:path';
import { randomUUID, createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { open, rm } from 'node:fs/promises';
import { OAuth } from './auth.mjs';
import { fail, check, bounded, json, remotePath, uri, IO } from './common.mjs';
export { AdapterError } from './common.mjs';

const API = 'https://www.googleapis.com/drive/v3/files';
export const FOLDER = 'application/vnd.google-apps.folder';
export const EXPORTS = {
  'application/vnd.google-apps.document': ['.docx', 'application/vnd.openxmlformats-officedocument.wordprocessingml.document'],
  'application/vnd.google-apps.spreadsheet': ['.xlsx', 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet'],
  'application/vnd.google-apps.presentation': ['.pptx', 'application/vnd.openxmlformats-officedocument.presentationml.presentation'],
};
const UPLOAD_TYPES = {
  '.docx': EXPORTS['application/vnd.google-apps.document'][1],
  '.xlsx': EXPORTS['application/vnd.google-apps.spreadsheet'][1],
  '.pptx': EXPORTS['application/vnd.google-apps.presentation'][1],
  '.pdf': 'application/pdf', '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg',
  '.gif': 'image/gif', '.webp': 'image/webp', '.avif': 'image/avif', '.svg': 'image/svg+xml',
  '.txt': 'text/plain', '.csv': 'text/csv', '.json': 'application/json', '.zip': 'application/zip',
  '.mp3': 'audio/mpeg', '.mp4': 'video/mp4',
};
const FIELDS = 'id,name,mimeType,size,modifiedTime,version,md5Checksum,parents,driveId,trashed,capabilities';
const child = (p, n) => path.posix.join(p, n);
export const revision = i => JSON.stringify([i.id, i.version ?? null, i.modifiedTime ?? null, i.md5Checksum ?? null]);
export function encodeName(name) {
  return !name || name === '.' || name === '..' || name.startsWith('~') || /[\x00-\x1f\x7f/\\]/u.test(name)
    ? '~e' + Buffer.from(name).toString('base64url') : name;
}
function decodeName(name) {
  if (name.startsWith('~i')) {
    const match = /^~i([A-Za-z0-9_-]+)~(.+)$/u.exec(name);
    if (!match) fail('invalid_input', 'Invalid item alias.');
    name = match[2];
  }
  if (!name.startsWith('~')) return name;
  if (!name.startsWith('~e')) fail('invalid_input', 'Reserved virtual name.');
  const match = /^(~e[A-Za-z0-9_-]*)(\.(?:docx|xlsx|pptx))?$/u.exec(name);
  if (!match) fail('invalid_input', 'Invalid escaped name.');
  const decoded = Buffer.from(match[1].slice(2), 'base64url').toString('utf8');
  if (encodeName(decoded) !== match[1]) fail('invalid_input', 'Invalid escaped name.');
  return decoded + (match[2] ?? '');
}
const displayName = i => encodeName(i.name) + (EXPORTS[i.mimeType]?.[0] ?? '');
export function virtualNames(items) {
  const counts = new Map();
  for (const i of items) counts.set(displayName(i), (counts.get(displayName(i)) ?? 0) + 1);
  return items.map(i => ({ ...i, virtual: counts.get(displayName(i)) > 1 ? `~i${Buffer.from(i.id).toString('base64url')}~${displayName(i)}` : displayName(i) }));
}

// New items cannot inherit an alias's Drive ID. This ordinary component is also
// the host transfer-name contract: full ID encoding avoids truncated-ID clashes.
export function transferName(component, item) {
  if (!component.startsWith('~i') && !component.startsWith('~e')) return component;
  const match = /^~i([A-Za-z0-9_-]+)~(.+)$/u.exec(component);
  if (component.startsWith('~i') && (!match || Buffer.from(match[1], 'base64url').toString('base64url') !== match[1])) fail('invalid_input', 'Invalid item alias.');
  const id = item ? Buffer.from(item.id).toString('base64url') : match?.[1];
  if (!id) fail('invalid_input', 'An escaped transfer name requires source metadata.');
  const display = match?.[2] ?? component, parsed = path.posix.parse(display);
  if (display.startsWith('~e')) {
    const raw = item?.name ?? decodeName(display);
    const ext = EXPORTS[item?.mimeType]?.[0] ?? path.posix.extname(raw);
    return `Drive-${id}${/^\.[^/\\\x00-\x1f\x7f]*$/u.test(ext) ? ext : ''}`;
  }
  return `${parsed.name}~drive-${id}${parsed.ext}`;
}
const transferPath = p => path.posix.basename(p).startsWith('~i') ? child(path.posix.dirname(p), transferName(path.posix.basename(p))) : p;

export class DriveAdapter {
  constructor(instance, readOnly, tempRoot, dependencies = {}) {
    Object.assign(this, { instance, readOnly, tempRoot });
    this.auth = dependencies.auth ?? new OAuth(instance, dependencies);
    this.exports = new Map();
  }
  async login(args, signal) { await this.auth.login(args, signal); }
  async response(url, options = {}, signal) {
    // Refresh only before issuing a request. Never replay even a 401 mutation.
    const token = await this.auth.accessToken(signal);
    const response = await this.auth.request(url, { ...options, headers: { ...options.headers, Authorization: `Bearer ${token}` } }, signal);
    if (!response.ok && response.status !== 308) {
      await response.body?.cancel();
      if (response.status === 401) { this.auth.access = null; fail('authentication_required', 'Google Drive authentication is required.'); }
      if (response.status === 403) fail('permission_denied', 'Google Drive denied this operation.');
      if (response.status === 404) fail('not_found', 'Remote item not found.');
      if (response.status === 409) fail('already_exists', 'A remote item already exists.');
      fail('io', IO);
    }
    return response;
  }
  async api(suffix = '', { method = 'GET', query = {}, body, headers = {} } = {}, signal) {
    const url = new URL(API + suffix); url.search = new URLSearchParams(query);
    const r = await this.response(url, { method, headers: { ...headers, ...(body ? { 'Content-Type': 'application/json' } : {}) }, body: body ? JSON.stringify(body) : undefined }, signal);
    if (r.status === 204) return null;
    return json(r);
  }
  supported(i, content = false) {
    if (i.driveId) fail('unsupported', 'Shared drives are not supported.');
    if (i.mimeType === 'application/vnd.google-apps.shortcut') fail('unsupported', 'Drive shortcuts are not followed.');
    if (content && i.mimeType?.startsWith('application/vnd.google-apps.') && i.mimeType !== FOLDER && !EXPORTS[i.mimeType])
      fail('unsupported', 'This Google Workspace document type cannot be exported.');
  }
  capability(i, name) { if (i.capabilities?.[name] !== true) fail('permission_denied', 'This item does not permit the requested operation.'); }
  async metadata(id, signal) { return this.api('/' + encodeURIComponent(id), { query: { fields: FIELDS } }, signal); }
  async children(parent, signal) {
    this.supported(parent);
    if (parent.mimeType !== FOLDER) fail('invalid_input', 'Expected a directory.');
    const items = [], seen = new Set(), ids = new Set(); let token;
    do {
      check(signal);
      const page = await this.api('', { query: { q: `'${parent.id.replaceAll('\\', '\\\\').replaceAll("'", "\\'")}' in parents and trashed = false`,
        spaces: 'drive', corpora: 'user', pageSize: '1000', fields: `nextPageToken,incompleteSearch,files(${FIELDS})`, ...(token ? { pageToken: token } : {}) } }, signal);
      if (page.incompleteSearch || !Array.isArray(page.files)) fail('io', 'Directory listing is incomplete.');
      for (const i of page.files) {
        if (typeof i.id !== 'string' || typeof i.name !== 'string' || ids.has(i.id)) fail('io', 'Invalid directory listing.');
        ids.add(i.id);
        if (!i.driveId) items.push(i);
      }
      token = page.nextPageToken;
      if (token && (typeof token !== 'string' || seen.has(token))) fail('io', 'Directory cursor loop.');
      seen.add(token);
      if (items.length > 100000) fail('unsupported', 'Directory exceeds the adapter listing limit.');
    } while (token);
    return virtualNames(items);
  }
  async lookup(p, signal) {
    let item = await this.metadata('root', signal);
    for (const name of p.split('/').filter(Boolean)) {
      const items = await this.children(item, signal);
      if (name.startsWith('~i')) {
        const match = /^~i([A-Za-z0-9_-]+)~(.+)$/u.exec(name);
        const id = match && Buffer.from(match[1], 'base64url').toString('utf8');
        item = match && items.find(i => i.id === id && displayName(i) === match[2] && Buffer.from(id).toString('base64url') === match[1]);
      } else item = items.find(i => i.virtual === name);
      if (!item) fail('not_found', 'Remote item not found. Refresh the directory.');
    }
    this.supported(item); return item;
  }
  entry(i, p, size = Number(i.size ?? 0)) {
    const basename = path.posix.basename(p), portable = p === '/' ? basename : transferName(basename, i);
    return { location: { connector: this.instance, id: uri(p) }, name: p === '/' ? '/' : path.posix.basename(p),
      transfer_name: portable !== basename ? portable : null,
      kind: i.mimeType === FOLDER ? 'directory' : 'file', navigable: i.mimeType === FOLDER, hidden: path.posix.basename(p).startsWith('.'),
      size, modified: i.modifiedTime ? Math.floor(Date.parse(i.modifiedTime) / 1000) : null,
      revision: revision(i),
      read_only: this.readOnly || i.capabilities?.canEdit !== true };
  }
  mutable(p) {
    if (this.readOnly || p === '/') fail('permission_denied', 'This location cannot be modified.');
  }
  async vacant(p, signal, movingId = null) {
    this.mutable(p);
    const parent = await this.lookup(path.posix.dirname(p), signal);
    this.capability(parent, 'canAddChildren');
    const base = path.posix.basename(p), name = decodeName(base);
    if (base.startsWith('~i') && (!movingId || base.split('~')[1] !== 'i' + Buffer.from(movingId).toString('base64url')))
      fail('invalid_input', 'Choose an ordinary destination name for a new item; a duplicate alias belongs to its existing Drive ID.');
    if ((await this.children(parent, signal)).some(i => i.virtual === base || displayName(i) === base || i.name === name))
      fail('already_exists', 'A remote item already exists at the destination.');
    return { parent, name };
  }
  async verifyDestination(parent, name, id, signal) {
    const matches = (await this.children(parent, signal)).filter(i => i.name === name);
    if (matches.length !== 1 || matches[0].id !== id) fail('already_exists', 'Concurrent Drive name collision. Both items have been retained; refresh the directory.');
  }
  async unchanged(item, signal) {
    if (revision(await this.metadata(item.id, signal)) !== revision(item)) fail('io', 'Source changed during the operation. Refresh before retrying.');
  }
  async exported(item, signal) {
    this.supported(item, true); this.capability(item, 'canDownload');
    const key = revision(item);
    if (this.exports.has(key)) return this.exports.get(key);
    // Keep the total export cache bounded as well as each individual export.
    if (this.exports.size >= 16) {
      const [oldKey, old] = this.exports.entries().next().value;
      await rm(old.file, { force: true }); this.exports.delete(oldKey);
    }
    const file = path.join(this.tempRoot, randomUUID()), handle = await open(file, 'wx', 0o600);
    let size = 0, success = false; const hash = createHash('sha256');
    try {
      const url = new URL(`${API}/${encodeURIComponent(item.id)}/export`); url.searchParams.set('mimeType', EXPORTS[item.mimeType][1]);
      const response = await this.response(url, {}, signal);
      for await (const chunk of response.body) {
        check(signal); size += chunk.length;
        if (size > 10 * 1024 * 1024) fail('unsupported', 'Google Workspace exports are limited to 10 MiB.');
        hash.update(chunk); await handle.writeFile(chunk);
      }
      await this.unchanged(item, signal);
      const result = { file, size, hash: hash.digest('hex') }; this.exports.set(key, result); success = true; return result;
    } finally { await handle.close(); if (!success) await rm(file, { force: true }); }
  }
  async read(item, offset, limit, signal) {
    this.supported(item, true); this.capability(item, 'canDownload');
    if (item.mimeType === FOLDER) fail('invalid_input', 'Only files can be read.');
    if (EXPORTS[item.mimeType]) {
      const exported = await this.exported(item, signal);
      const handle = await open(exported.file, 'r');
      try { const buffer = Buffer.alloc(Math.min(limit, Math.max(0, exported.size - offset))); const { bytesRead } = await handle.read(buffer, 0, buffer.length, offset); return buffer.subarray(0, bytesRead); }
      finally { await handle.close(); }
    }
    const size = Number(item.size);
    if (!Number.isSafeInteger(size) || size < 0) fail('unsupported', 'Invalid remote file size.');
    if (!limit || offset >= size) return Buffer.alloc(0);
    const end = Math.min(size, offset + limit) - 1;
    const r = await this.response(`${API}/${encodeURIComponent(item.id)}?alt=media`, { headers: { Range: `bytes=${offset}-${end}` } }, signal);
    if (r.status !== 206 || r.headers.get('content-range') !== `bytes ${offset}-${end}/${size}`) {
      await r.body?.cancel(); fail('io', 'Drive returned an invalid byte range.');
    }
    const data = await bounded(r, end - offset + 1);
    if (data.length !== end - offset + 1) fail('io', 'Drive returned a truncated byte range.');
    await this.unchanged(item, signal); return data;
  }
  async mkdir(p, signal) {
    const { parent, name } = await this.vacant(p, signal);
    const item = await this.api('', { method: 'POST', query: { fields: FIELDS }, body: { name, mimeType: FOLDER, parents: [parent.id] } }, signal);
    await this.verifyDestination(parent, name, item.id, signal); return item;
  }
  async move(src, dst, signal) {
    this.mutable(src);
    if (dst === src || dst.startsWith(src + '/')) fail('invalid_input', 'Cannot move an item into itself.');
    const item = await this.lookup(src, signal), { parent, name } = await this.vacant(dst, signal, item.id);
    this.capability(item, 'canRename');
    if (!item.parents?.includes(parent.id)) this.capability(item, 'canMoveItemWithinDrive');
    await this.unchanged(item, signal);
    const exportExt = EXPORTS[item.mimeType]?.[0];
    if (exportExt && !name.endsWith(exportExt)) fail('invalid_input', 'Exported documents must retain their virtual extension.');
    const remoteName = exportExt ? name.slice(0, -exportExt.length) : name;
    await this.api('/' + encodeURIComponent(item.id), { method: 'PATCH', query: { fields: FIELDS,
      ...(!item.parents?.includes(parent.id) ? { addParents: parent.id, removeParents: (item.parents ?? []).join(',') } : {}) }, body: { name: remoteName } }, signal);
    await this.verifyDestination(parent, remoteName, item.id, signal);
  }
  async upload(dst, source, signal, progress = () => {}) {
    const { parent } = await this.vacant(dst, signal);
    if (typeof source !== 'string' || !path.isAbsolute(source)) fail('invalid_input', 'An absolute private spool path is required.');
    const handle = await open(source, constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const stat = await handle.stat();
      if (!stat.isFile() || stat.uid !== process.getuid() || (stat.mode & 0o077) || !Number.isSafeInteger(stat.size)) fail('permission_denied', 'Upload spool must be an owner-only regular file.');
      const staging = `.niri-transfer-${randomUUID()}${path.posix.extname(dst)}`;
      const mediaType = UPLOAD_TYPES[path.posix.extname(dst).toLowerCase()] ?? 'application/octet-stream';
      const response = await this.response('https://www.googleapis.com/upload/drive/v3/files?uploadType=resumable&fields=' + encodeURIComponent(FIELDS), {
        method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Upload-Content-Length': String(stat.size), 'X-Upload-Content-Type': mediaType },
        // Omission permits Drive content detection, without requesting native import.
        body: JSON.stringify({ name: staging, parents: [parent.id] }),
      }, signal);
      const location = response.headers.get('location'); await response.body?.cancel();
      let url; try { url = new URL(location); } catch { fail('io', IO); }
      if (url.origin !== 'https://www.googleapis.com' || url.pathname !== '/upload/drive/v3/files' || url.username || url.password || url.hash) fail('io', 'Invalid upload session.');
      let offset = 0, item;
      do {
        check(signal);
        const buffer = Buffer.alloc(Math.min(1048576, stat.size - offset));
        const { bytesRead } = await handle.read(buffer, 0, buffer.length, offset);
        if (bytesRead !== buffer.length) fail('io', 'Upload spool changed.');
        const end = offset + buffer.length;
        const r = await this.response(url, { method: 'PUT', headers: { 'Content-Type': mediaType, 'Content-Length': String(buffer.length),
          'Content-Range': stat.size ? `bytes ${offset}-${end - 1}/${stat.size}` : 'bytes */0' }, body: buffer }, signal);
        if (end < stat.size) {
          if (r.status !== 308 || r.headers.get('range') !== `bytes=0-${end - 1}`) { await r.body?.cancel(); fail('io', IO); }
          await r.body?.cancel();
        } else {
          if (![200, 201].includes(r.status)) { await r.body?.cancel(); fail('io', IO); }
          item = await json(r);
        }
        await progress(buffer.length); offset = end;
      } while (offset < stat.size);
      if (!item?.id) fail('io', IO);
      const after = await handle.stat();
      if (after.size !== stat.size || after.mtimeMs !== stat.mtimeMs) fail('io', 'Upload spool changed; staging item retained.');
      const { name, parent: finalParent } = await this.vacant(dst, signal);
      if (finalParent.id !== parent.id) fail('io', 'Destination changed; staging item retained.');
      await this.api('/' + encodeURIComponent(item.id), { method: 'PATCH', query: { fields: FIELDS }, body: { name } }, signal);
      await this.verifyDestination(parent, name, item.id, signal);
      await progress({ items: 1 });
    } finally { await handle.close(); }
  }
  async copy(src, dst, signal, progress = () => {}) {
    dst = transferPath(dst);
    if (src === dst || dst.startsWith(src + '/')) fail('invalid_input', 'Cannot copy an item into itself.');
    const item = await this.lookup(src, signal);
    if (item.mimeType !== FOLDER) this.capability(item, 'canCopy');
    await this.vacant(dst, signal);
    if (item.mimeType === FOLDER) {
      const children = await this.children(item, signal); await this.mkdir(dst, signal);
      await progress({ items: 1 });
      for (const i of children) {
        // A real sibling can already have a generated transfer name. Select a
        // vacant name before each copy rather than losing that sibling.
        await this.run('copy_unique', { path: uri(child(src, i.virtual)), destination: uri(child(dst, transferName(i.virtual, i))) }, signal, progress);
      }
    } else {
      this.supported(item, true);
      const { parent, name } = await this.vacant(dst, signal);
      const ext = EXPORTS[item.mimeType]?.[0];
      if (ext && !name.endsWith(ext)) fail('invalid_input', 'Native document copies must retain their virtual extension.');
      const remoteName = ext ? name.slice(0, -ext.length) : name;
      await this.unchanged(item, signal);
      const copied = await this.api('/' + encodeURIComponent(item.id) + '/copy', { method: 'POST', query: { fields: FIELDS }, body: { name: remoteName, parents: [parent.id] } }, signal);
      await this.unchanged(item, signal); await this.verifyDestination(parent, remoteName, copied.id, signal);
      await progress({ items: 1 });
    }
  }
  async run(op, args, signal, progress = () => {}) {
    check(signal); const p = remotePath(args.path);
    switch (op) {
      case 'list': return { entries: (await this.children(await this.lookup(p, signal), signal)).map(i => this.entry(i, child(p, i.virtual))), next: null };
      case 'stat': {
        const item = await this.lookup(p, signal); this.supported(item, true);
        return this.entry(item, p, EXPORTS[item.mimeType] ? (await this.exported(item, signal)).size : Number(item.size ?? 0));
      }
      case 'read': {
        if (!Number.isSafeInteger(args.offset) || args.offset < 0 || !Number.isInteger(args.limit) || args.limit < 0 || args.limit > 1048576) fail('invalid_input', 'Invalid bounded read.');
        const bytes = await this.read(await this.lookup(p, signal), args.offset, args.limit, signal); await progress(bytes.length); return [...bytes];
      }
      case 'write_new': await this.upload(p, args.source, signal, progress); return null;
      case 'create_directory': await this.mkdir(p, signal); return null;
      case 'rename': case 'move': case 'move_item': await this.move(p, remotePath(args.destination), signal); return null;
      case 'copy': await this.copy(p, remotePath(args.destination), signal, progress); return null;
      case 'copy_unique': {
        const requested = remotePath(args.destination), source = await this.lookup(p, signal);
        let dst = child(path.posix.dirname(requested), transferName(path.posix.basename(requested), source));
        const parsed = path.posix.parse(dst);
        for (let n = 1; ; n++) {
          try { await this.vacant(dst, signal); break; } catch (e) { if (e.code !== 'already_exists') throw e; }
          dst = child(parsed.dir, `${parsed.name}-${n}${parsed.ext}`);
        }
        await this.copy(p, dst, signal, progress); return { connector: this.instance, id: uri(dst) };
      }
      case 'delete': {
        this.mutable(p); const item = await this.lookup(p, signal);
        if (!['trash', 'permanent'].includes(args.mode)) fail('invalid_input', 'Invalid delete mode.');
        this.capability(item, args.mode === 'trash' ? 'canTrash' : 'canDelete'); await this.unchanged(item, signal);
        await this.api('/' + encodeURIComponent(item.id), { method: args.mode === 'trash' ? 'PATCH' : 'DELETE', ...(args.mode === 'trash' ? { body: { trashed: true }, query: { fields: FIELDS } } : {}) }, signal); return null;
      }
      case 'archive': {
        this.mutable(p);
        if (path.posix.basename(p) === '.archive' || path.posix.basename(path.posix.dirname(p)) === '.archive') fail('invalid_input', 'This item is already an archive.');
        const dir = child(path.posix.dirname(p), '.archive');
        try { await this.lookup(dir, signal); } catch (e) { if (e.code !== 'not_found') throw e; await this.mkdir(dir, signal); }
        const dst = child(dir, path.posix.basename(p)); await this.move(p, dst, signal); return { connector: this.instance, id: uri(dst) };
      }
      case 'restore': {
        if (path.posix.basename(path.posix.dirname(p)) !== '.archive') fail('invalid_input', 'This item is not in a sibling .archive directory.');
        const dst = child(path.posix.dirname(path.posix.dirname(p)), path.posix.basename(p)); await this.move(p, dst, signal); return { connector: this.instance, id: uri(dst) };
      }
      default: fail('unsupported', 'Unknown Google Drive operation.');
    }
  }
}
