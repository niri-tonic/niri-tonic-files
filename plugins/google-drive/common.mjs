// SPDX-License-Identifier: GPL-3.0-only
export class AdapterError extends Error {
  constructor(code, message) { super(message); this.code = code; }
}
export const fail = (code, message) => { throw new AdapterError(code, message); };
export const check = signal => { if (signal?.aborted) fail('cancelled', 'Cancelled. Completed items have been kept.'); };
export const IO = 'Google Drive operation failed. Refresh before retrying; completed items or .niri-transfer files may remain.';
export async function bounded(response, maximum) {
  const chunks = []; let size = 0;
  for await (const chunk of response.body ?? []) {
    size += chunk.length;
    if (size > maximum) fail('io', 'Response exceeded its size limit.');
    chunks.push(Buffer.from(chunk));
  }
  return Buffer.concat(chunks);
}
export const json = async response => JSON.parse((await bounded(response, 8 * 1024 * 1024)).toString('utf8'));
export function remotePath(value) {
  if (typeof value !== 'string' || !value.startsWith('file:///') || /[?#]/u.test(value)) fail('invalid_input', 'Expected an absolute file URI.');
  let p;
  try { p = decodeURIComponent(value.slice(7)); } catch { fail('invalid_input', 'Invalid file URI.'); }
  if (p !== '/' && p.split('/').slice(1).some(n => !n || n === '.' || n === '..' || /[\x00-\x1f\x7f\\]/u.test(n))) fail('invalid_input', 'Invalid virtual path.');
  return p;
}
export const uri = p => 'file://' + p.split('/').map(encodeURIComponent).join('/');
