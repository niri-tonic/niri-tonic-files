// SPDX-License-Identifier: GPL-3.0-only
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, readFile, stat, chmod, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { OAuth, loadSession, saveSession, SCOPE } from '../auth.mjs';

const response = value => new Response(JSON.stringify(value));
async function directory(t) {
  const dir = await mkdtemp(path.join(tmpdir(), 'drive-auth-test-'));
  t.after(() => rm(dir, { recursive: true, force: true })); return dir;
}
const args = file => ({ client_id: 'client', token_path: file, persist_session: true });

test('loopback verifies state, exact callback and PKCE; persists refresh only', async t => {
  const file = path.join(await directory(t), 'session.json'); let authorization, exchanges = 0;
  const oauth = new OAuth('one', {
    browser: async value => {
      authorization = new URL(value);
      assert.equal(authorization.origin, 'https://accounts.google.com');
      assert.equal(authorization.searchParams.get('scope'), SCOPE);
      assert.equal(authorization.searchParams.get('code_challenge_method'), 'S256');
      const callback = new URL(authorization.searchParams.get('redirect_uri'));
      assert.equal(callback.hostname, '127.0.0.1');
      callback.search = new URLSearchParams({ state: 'wrong', code: 'secret-code' });
      assert.equal((await fetch(callback)).status, 400);
      callback.searchParams.set('state', authorization.searchParams.get('state'));
      callback.pathname = '/wrong'; assert.equal((await fetch(callback)).status, 400);
      callback.pathname = '/'; assert.equal((await fetch(callback)).status, 200);
    },
    fetch: async (url, options) => {
      exchanges++; assert.equal(url, 'https://oauth2.googleapis.com/token');
      assert.equal(options.body.get('code'), 'secret-code');
      assert.equal(createHash('sha256').update(options.body.get('code_verifier')).digest('base64url'), authorization.searchParams.get('code_challenge'));
      return response({ access_token: 'secret-access', refresh_token: 'secret-refresh', expires_in: 3600, scope: SCOPE });
    },
  });
  await oauth.login(args(file)); assert.equal(exchanges, 1);
  const saved = await readFile(file, 'utf8'); assert.ok(saved.includes('secret-refresh'));
  assert.ok(!saved.includes('secret-access')); assert.ok(!saved.includes('secret-code'));
  assert.equal((await stat(file)).mode & 0o777, 0o600);
  await assert.rejects(fetch(authorization.searchParams.get('redirect_uri')));
});

test('refresh reuses credentials, rotates atomically, binds instance/client, and never opens browser on revocation', async t => {
  const file = path.join(await directory(t), 'session.json');
  await saveSession(file, 'client', 'one', 'old-refresh');
  let now = 1000, requests = 0;
  const oauth = new OAuth('one', { now: () => now, browser: () => assert.fail('interactive retry'), fetch: async (_url, options) => {
    requests++; assert.equal(options.body.get('grant_type'), 'refresh_token');
    assert.equal(options.body.get('refresh_token'), requests === 1 ? 'old-refresh' : 'rotated');
    if (requests === 3) return new Response('secret-refresh revoked secret-code', { status: 400 });
    return response({ access_token: 'access', expires_in: 3600, ...(requests === 1 ? { refresh_token: 'rotated' } : {}) });
  } });
  await oauth.login(args(file)); assert.equal(await loadSession(file, 'client', 'one'), 'rotated');
  now += 3600000; await oauth.accessToken();
  now += 3600000; await assert.rejects(oauth.accessToken(), e => e.code === 'authentication_required' && !e.message.includes('secret'));
  await assert.rejects(loadSession(file, 'client', 'two'), { code: 'authentication_required' });
  await assert.rejects(loadSession(file, 'other', 'one'), { code: 'authentication_required' });
});

test('session rejects insecure modes, symlinks, and nonprivate parents', async t => {
  const dir = await directory(t), file = path.join(dir, 'session');
  await saveSession(file, 'client', 'one', 'refresh'); await chmod(file, 0o644);
  await assert.rejects(loadSession(file, 'client', 'one'), { code: 'permission_denied' });
  await assert.rejects(saveSession(file, 'client', 'one', 'new'), { code: 'permission_denied' });
  await chmod(file, 0o600); await symlink(file, path.join(dir, 'link'));
  await assert.rejects(loadSession(path.join(dir, 'link'), 'client', 'one'), { code: 'permission_denied' });
  await chmod(dir, 0o755); await assert.rejects(loadSession(file, 'client', 'one'), { code: 'permission_denied' });
});

test('cancellation and deadline close loopback listener', async t => {
  const file = path.join(await directory(t), 'unused');
  for (const timeout of [false, true]) {
    const controller = new AbortController(); let callback;
    const oauth = new OAuth('one', { authTimeout: timeout ? 25 : 5000, browser: async url => {
      callback = new URL(url).searchParams.get('redirect_uri'); if (!timeout) controller.abort();
    }, fetch: () => assert.fail('no exchange') });
    await assert.rejects(oauth.login({ ...args(file), persist_session: false }, controller.signal), e => ['cancelled', 'authentication_required'].includes(e.code));
    await assert.rejects(fetch(callback));
  }
});

test('separate account files do not share refresh credentials', async t => {
  const dir = await directory(t);
  for (const instance of ['one', 'two']) {
    const file = path.join(dir, instance); await saveSession(file, 'client', instance, instance + '-refresh');
    const oauth = new OAuth(instance, { browser: () => assert.fail(), fetch: async (_url, options) => {
      assert.equal(options.body.get('refresh_token'), instance + '-refresh'); return response({ access_token: instance, expires_in: 3600 });
    } });
    await oauth.login(args(file)); assert.equal(await oauth.accessToken(), instance);
  }
});

test('browser and network failures are sanitized; network deadline aborts', async t => {
  const file = path.join(await directory(t), 'session');
  const broken = new OAuth('one', { browser: async () => { throw new Error('secret'); } });
  await assert.rejects(broken.login({ ...args(file), persist_session: false }), e => e.code === 'authentication_required' && !e.message.includes('secret'));
  await saveSession(file, 'client', 'one', 'refresh');
  const oauth = new OAuth('one', { timeout: 10, fetch: (_url, { signal }) => new Promise((_resolve, reject) => {
    const timer = setTimeout(() => assert.fail('deadline failed'), 1000);
    signal.addEventListener('abort', () => { clearTimeout(timer); reject(new Error('secret')); }, { once: true });
  }) });
  await assert.rejects(oauth.login(args(file)), e => e.code === 'io' && !e.message.includes('secret'));
});
