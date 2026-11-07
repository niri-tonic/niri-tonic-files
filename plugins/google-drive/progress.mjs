// SPDX-License-Identifier: GPL-3.0-only
const PHASES = new Set(['list', 'stat', 'read', 'write_new', 'create_directory', 'rename', 'move', 'move_item', 'copy', 'copy_unique', 'delete', 'archive', 'restore']);

// One in-flight stdout write, no queue. Awaited work callbacks apply backpressure;
// the timer sends coalesced heartbeats while a long API/listing call is pending.
export async function withProgress(id, op, send, work, { interval = 1000, now = Date.now, onError = () => {} } = {}) {
  let bytes = 0, items = 0, last = -Infinity, pending, failure, stopped = false;
  const add = (total, delta) => Number.isSafeInteger(delta) && delta >= 0 ? Math.min(Number.MAX_SAFE_INTEGER, total + delta) : total;
  async function emit(force = false) {
    if (pending) await pending;
    if (failure) throw failure;
    if (!force && now() - last < interval) return;
    last = now();
    pending = Promise.resolve().then(() => send({ id, progress: { phase: PHASES.has(op) ? op : 'operation', bytes, items, cancellable: true } }))
      .catch(error => { failure = error; onError(error); });
    await pending; pending = undefined;
    if (failure) throw failure;
  }
  const progress = async delta => {
    bytes = add(bytes, typeof delta === 'number' ? delta : delta?.bytes);
    items = add(items, typeof delta === 'object' ? delta?.items : 0);
    await emit();
  };
  const timer = setInterval(() => {
    // Never queue another waiter behind a blocked stdout write.
    if (!stopped && !pending && !failure) void emit().catch(() => {});
  }, interval);
  try {
    const result = await work(progress);
    stopped = true; clearInterval(timer);
    await emit(true);
    return result;
  } finally {
    stopped = true; clearInterval(timer); await pending;
  }
}
