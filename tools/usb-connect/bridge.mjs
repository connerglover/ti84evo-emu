import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';

export function startBridge(executable, bundle, { flashFile, provision = false } = {}) {
  const args = ['usb-bridge', bundle];
  if (flashFile) args.push('--flash-file', flashFile);
  if (provision) args.push('--provision');
  const child = spawn(executable, args, { windowsHide: true, stdio: ['pipe', 'pipe', 'inherit'] });
  const waiting = [];
  let failure;
  const fail = error => { failure = error; for (const p of waiting.splice(0)) p.reject(error); };
  child.on('error', fail);
  const completed = new Promise(resolve => {
    child.once('error', error => resolve({ error }));
    // 'close' follows stdout drainage, so the final response is not discarded.
    child.once('close', (code, signal) => {
      fail(new Error(`USB bridge exited (${signal || code})`));
      resolve({ code, signal });
    });
  });
  child.stdin.on('error', fail);
  createInterface({ input: child.stdout }).on('line', line => {
    const pending = waiting.shift();
    if (!pending) return fail(new Error('unexpected USB bridge response'));
    try {
      const response = JSON.parse(line);
      if (response.ok) pending.resolve(response.data);
      else pending.reject(new Error(response.error));
    } catch (error) { pending.reject(error); }
  });
  const request = data => new Promise((resolve, reject) => {
    if (failure) return reject(failure);
    waiting.push({ resolve, reject });
    child.stdin.write(JSON.stringify(data) + '\n');
  });
  return { request, child, async close() {
    let requestError;
    try { if (!failure) await request({ op: 'quit' }); }
    catch (error) { requestError = error; }
    finally { child.stdin.end(); }
    // Quit is acknowledged before the CLI writes flash. Only process success
    // confirms that persistence succeeded.
    const result = await completed;
    if (result.error) throw result.error;
    if (result.code !== 0) throw new Error(`USB bridge exited (${result.signal || result.code}); flash may not have been saved`);
    if (requestError) throw requestError;
  } };
}
