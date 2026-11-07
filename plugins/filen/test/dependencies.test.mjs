// SPDX-License-Identifier: AGPL-3.0-only
import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { generateKeyPairSync, createPublicKey, createPrivateKey, createECDH } from 'node:crypto';
import { FilenSDK } from '@filen/sdk';
const require = createRequire(import.meta.url);
const sdkRequire = createRequire(require.resolve('@filen/sdk'));

test('patched Axios preserves the SDK instance interface without any transport', async () => {
  const axios = sdkRequire('axios');
  assert.equal(axios.VERSION, '0.34.0');
  const sdk = new FilenSDK({ connectToSocket: false, metadataCache: false });
  let calls = 0;
  sdk.axiosInstance.defaults.adapter = async config => {
    calls++;
    assert.equal(config.headers.Authorization, 'Bearer synthetic-fixture');
    assert.equal(config.maxRedirects, 0);
    assert.equal(config.signal.aborted, false);
    if (config.method === 'post') assert.deepEqual(JSON.parse(config.data), { fixture: true });
    return { data: '{"fixture":true}', status: 200, statusText: 'OK', headers: {}, config };
  };
  const options = { headers: { Authorization: 'Bearer synthetic-fixture' }, signal: new AbortController().signal,
    timeout: 100, responseType: 'json', maxRedirects: 0, maxBodyLength: Infinity, maxContentLength: Infinity };
  assert.deepEqual((await sdk.axiosInstance.post('https://fixture.invalid/isolated', { fixture: true }, options)).data, { fixture: true });
  assert.deepEqual((await sdk.axiosInstance.get('https://fixture.invalid/isolated', options)).data, { fixture: true });
  const cancelled = new AbortController(); cancelled.abort();
  await assert.rejects(sdk.axiosInstance.get('https://fixture.invalid/isolated', { ...options, signal: cancelled.signal }), { code: 'ERR_CANCELED' });
  assert.equal(calls, 2);
});

test('SDK DER/PEM conversion and elliptic key derivation remain compatible', async () => {
  const { derKeyToPem } = sdkRequire('./crypto/utils.js');
  const keyutil = sdkRequire('js-crypto-key-utils');
  const keyRequire = createRequire(sdkRequire.resolve('js-crypto-key-utils'));
  assert.equal(keyRequire('elliptic/package.json').version, '6.6.1');
  // Fresh disposable keys only; no account credentials or persisted key material.
  const pair = generateKeyPairSync('rsa', { modulusLength: 1024 });
  for (const [key, type, importKey] of [[pair.publicKey, 'spki', createPublicKey], [pair.privateKey, 'pkcs8', createPrivateKey]]) {
    const der = key.export({ type, format: 'der' });
    const pem = await derKeyToPem({ key: der.toString('base64') });
    assert.deepEqual(importKey(pem).export({ type, format: 'der' }), der);
  }
  const ec = createECDH('prime256v1'); ec.generateKeys();
  const key = new keyutil.Key('oct', new Uint8Array(ec.getPrivateKey()), { namedCurve: 'P-256' });
  const jwk = await key.export('jwk');
  const publicOctets = ec.getPublicKey();
  assert.deepEqual(Buffer.from(jwk.x, 'base64url'), publicOctets.subarray(1, 33));
  assert.deepEqual(Buffer.from(jwk.y, 'base64url'), publicOctets.subarray(33));
});
