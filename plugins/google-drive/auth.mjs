// SPDX-License-Identifier: GPL-3.0-only
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { randomBytes, createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { lstat, mkdir, open, rename, rm } from 'node:fs/promises';
import path from 'node:path';
import { fail, check, json, AdapterError, IO } from './common.mjs';

export const SCOPE = 'https://www.googleapis.com/auth/drive';
const AUTH = 'Google Drive authentication is required. Check the OAuth client and saved session.';
const privateStat = (s, directory = false) => {
  if (s.uid !== process.getuid() || (s.mode & 0o077) || (directory ? !s.isDirectory() : !s.isFile()) || (!directory && s.nlink !== 1))
    fail('permission_denied', 'Session storage must be private, owner-only, and not a symlink.');
};
export async function sessionParent(file) {
  if (typeof file !== 'string' || !path.isAbsolute(file) || path.normalize(file) !== file) fail('invalid_input', 'An absolute session path is required.');
  const parent = path.dirname(file);
  // Check every ancestor for symlinks before creating the private leaf directory.
  let current = '/';
  for (const part of parent.split('/').filter(Boolean)) {
    current = path.join(current, part);
    try { if (!(await lstat(current)).isDirectory()) fail('permission_denied', 'Invalid session directory.'); }
    catch (e) { if (e.code !== 'ENOENT') throw e; await mkdir(current, { mode: 0o700 }); }
  }
  privateStat(await lstat(parent), true);
}
export async function loadSession(file, client, instance) {
  await sessionParent(file);
  let handle;
  try {
    handle = await open(file, constants.O_RDONLY | constants.O_NOFOLLOW);
    const s = await handle.stat(); privateStat(s);
    if (s.size > 16384) fail('permission_denied', 'Invalid saved session.');
    const token = JSON.parse(await handle.readFile('utf8'));
    if (token.version !== 1 || token.client_id !== client || token.instance !== instance || typeof token.refresh_token !== 'string' || !token.refresh_token)
      fail('authentication_required', AUTH);
    return token.refresh_token;
  } catch (e) {
    if (e.code === 'ENOENT') return null;
    if (e instanceof AdapterError) throw e;
    fail('permission_denied', 'Cannot read the private saved session.');
  } finally { await handle?.close(); }
}
export async function saveSession(file, client, instance, refresh) {
  await sessionParent(file);
  try { privateStat(await lstat(file)); } catch (e) { if (e.code !== 'ENOENT') throw e; }
  const temp = `${file}.${randomBytes(16).toString('hex')}.tmp`;
  let handle;
  try {
    handle = await open(temp, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
    await handle.writeFile(JSON.stringify({ version: 1, client_id: client, instance, refresh_token: refresh }));
    await handle.sync(); await handle.close(); handle = null;
    await rename(temp, file);
  } finally { await handle?.close(); await rm(temp, { force: true }); }
}
export function openBrowser(url, signal) {
  return new Promise((resolve, reject) => {
    const child = spawn('xdg-open', [url], { stdio: 'ignore', shell: false, signal });
    child.once('error', () => reject(new AdapterError('authentication_required', 'Cannot open the system browser.')));
    child.once('exit', code => code === 0 ? resolve() : reject(new AdapterError('authentication_required', 'Cannot open the system browser.')));
  });
}

export class OAuth {
  constructor(instance, { fetch: fetcher = fetch, browser = openBrowser, now = Date.now, timeout = 30000, authTimeout = 300000 } = {}) {
    Object.assign(this, { instance, fetcher, browser, now, timeout, authTimeout });
  }
  async request(url, options, signal) {
    check(signal);
    const combined = AbortSignal.any([...(signal ? [signal] : []), AbortSignal.timeout(this.timeout)]);
    try { return await this.fetcher(url, { ...options, redirect: 'error', signal: combined }); }
    catch { check(signal); fail('io', IO); }
  }
  async token(parameters, signal) {
    const body = new URLSearchParams({ ...parameters, client_id: this.client });
    if (this.secret) body.set('client_secret', this.secret);
    const response = await this.request('https://oauth2.googleapis.com/token', { method: 'POST', body }, signal);
    if (!response.ok) { await response.body?.cancel(); this.access = null; fail('authentication_required', AUTH); }
    let result;
    try { result = await json(response); } catch { fail('authentication_required', AUTH); }
    if (typeof result.access_token !== 'string' || !result.access_token || !Number.isFinite(result.expires_in) || result.expires_in <= 0 ||
      (result.scope && !result.scope.split(' ').includes(SCOPE))) fail('authentication_required', AUTH);
    const refresh = result.refresh_token || this.refresh;
    if (typeof refresh !== 'string' || !refresh) fail('authentication_required', AUTH);
    if (this.persist && refresh !== this.refresh) await saveSession(this.file, this.client, this.instance, refresh);
    this.refresh = refresh; this.access = result.access_token; this.expires = this.now() + result.expires_in * 1000;
  }
  async login(args, signal) {
    if (typeof args.client_id !== 'string' || !args.client_id || (args.client_secret !== undefined && typeof args.client_secret !== 'string') || typeof args.persist_session !== 'boolean')
      fail('invalid_input', 'Invalid OAuth login arguments.');
    this.client = args.client_id; this.secret = args.client_secret; this.file = args.token_path; this.persist = args.persist_session;
    if (typeof this.file !== 'string' || !path.isAbsolute(this.file)) fail('invalid_input', 'An absolute session path is required.');
    this.refresh = this.persist ? await loadSession(this.file, this.client, this.instance) : null;
    if (this.refresh) await this.token({ grant_type: 'refresh_token', refresh_token: this.refresh }, signal);
    else await this.authorize(signal);
  }
  async accessToken(signal) {
    if (!this.refresh) fail('authentication_required', AUTH);
    if (!this.access || this.expires <= this.now() + 60000) await this.token({ grant_type: 'refresh_token', refresh_token: this.refresh }, signal);
    return this.access;
  }
  async authorize(signal) {
    const deadline = AbortSignal.any([...(signal ? [signal] : []), AbortSignal.timeout(this.authTimeout)]);
    const browserController = new AbortController();
    check(deadline);
    const state = randomBytes(32).toString('base64url'), verifier = randomBytes(32).toString('base64url');
    let redirect, resolveCode, rejectCode;
    const code = new Promise((resolve, reject) => { resolveCode = resolve; rejectCode = reject; });
    // A browser-launch failure can race with abort; keep the callback promise handled.
    code.catch(() => {});
    const server = createServer({ maxHeaderSize: 8192 }, (req, res) => {
      res.setHeader('Content-Type', 'text/plain'); res.setHeader('Cache-Control', 'no-store');
      let url;
      try { url = new URL(req.url, redirect); } catch { res.writeHead(400); res.end('Invalid callback.'); return; }
      if (req.method !== 'GET' || req.headers.host !== new URL(redirect).host || url.origin !== new URL(redirect).origin || url.pathname !== '/' ||
          url.searchParams.getAll('state').length !== 1 || url.searchParams.get('state') !== state) {
        res.writeHead(400); res.end('Invalid callback.'); return;
      }
      if (url.searchParams.has('error') || url.searchParams.getAll('code').length !== 1 || !url.searchParams.get('code')) {
        res.end('Authorization declined. Return to the application.'); rejectCode(new AdapterError('authentication_required', AUTH)); return;
      }
      res.end('Authorization received. You can close this tab.'); resolveCode(url.searchParams.get('code'));
    });
    server.requestTimeout = 5000; server.headersTimeout = 5000;
    const aborted = () => { server.closeAllConnections(); server.close(); rejectCode(new AdapterError(signal?.aborted ? 'cancelled' : 'authentication_required', 'Authorization cancelled or timed out.')); };
    deadline.addEventListener('abort', aborted, { once: true });
    try {
      await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
      check(deadline);
      redirect = `http://127.0.0.1:${server.address().port}/`;
      const url = new URL('https://accounts.google.com/o/oauth2/v2/auth');
      url.search = new URLSearchParams({ client_id: this.client, redirect_uri: redirect, response_type: 'code', scope: SCOPE,
        state, code_challenge: createHash('sha256').update(verifier).digest('base64url'), code_challenge_method: 'S256', access_type: 'offline', prompt: 'consent select_account' });
      await Promise.race([this.browser(url.href, AbortSignal.any([deadline, browserController.signal])), code.then(() => {})]);
      const authorizationCode = await code;
      await this.token({ grant_type: 'authorization_code', code: authorizationCode, code_verifier: verifier, redirect_uri: redirect }, deadline);
    } catch (e) {
      check(signal);
      if (e instanceof AdapterError) throw e;
      fail('authentication_required', AUTH);
    } finally {
      browserController.abort();
      deadline.removeEventListener('abort', aborted); server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
    }
  }
}
