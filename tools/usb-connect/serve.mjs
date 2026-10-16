// Local browser front end for the official TI Connect communications library.
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { parseArgs } from 'node:util';
import { startBridge } from './bridge.mjs';

const { values } = parseArgs({ options: {
  cli: { type: 'string' }, bundle: { type: 'string' }, flash: { type: 'string' },
  provision: { type: 'boolean' }, port: { type: 'string', default: '0' },
  library: { type: 'string' },
} });
if (!values.cli || !values.bundle) throw new Error('--cli and --bundle are required');
const bridge = startBridge(values.cli, values.bundle, { flashFile: values.flash, provision: values.provision });
const assets = new Map(await Promise.all(['index.html', 'client.mjs', 'usb-host.mjs', 'client-environment.mjs'].map(async name =>
  ['/' + name, await readFile(new URL(name, import.meta.url))])));
// Fetch into memory at launch or use the user's local copy. Never vendor TI code.
for (const name of ['ticonnect.min.js', 'ticonnect.js', 'ticonnect.worker.js']) {
  let source;
  if (values.library) {
    const { join } = await import('node:path');
    source = await readFile(join(values.library, name));
  } else {
    const response = await fetch(`https://connectevo.ti.com/ticevo/en/assets/library/${name}`, { signal: AbortSignal.timeout(30_000) });
    if (!response.ok) throw new Error(`TI library download failed: ${response.status}`);
    source = Buffer.from(await response.arrayBuffer());
  }
  assets.set('/library/' + name, source);
}
let origin;
const server = createServer(async (req, res) => {
  try {
    res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
    // No CORS or DNS rebinding access: only this loopback origin can drive the guest.
    if (req.headers.host !== new URL(origin).host || (req.headers.origin && req.headers.origin !== origin)) {
      res.writeHead(403).end(); return;
    }
    if (req.method === 'POST' && req.url === '/usb' && req.headers['content-type'] === 'application/json') {
      let body = '';
      for await (const chunk of req) {
        body += chunk;
        if (body.length > 16384) { res.writeHead(413).end(); return; }
      }
      let result;
      try { result = { ok: true, data: await bridge.request(JSON.parse(body)) }; }
      catch (error) { result = { ok: false, error: error.message }; }
      res.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' }).end(JSON.stringify(result));
    } else if (req.method === 'GET' && assets.has(req.url === '/' ? '/index.html' : req.url)) {
      const path = req.url === '/' ? '/index.html' : req.url;
      res.writeHead(200, { 'Content-Type': path.endsWith('.html') ? 'text/html; charset=utf-8' : 'text/javascript; charset=utf-8',
        'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' }).end(assets.get(path));
    } else res.writeHead(404).end();
  } catch (error) { res.writeHead(500).end(error.message); }
});
server.listen(Number(values.port), '127.0.0.1', () => {
  origin = `http://127.0.0.1:${server.address().port}`;
  console.log(`Open ${origin} in your browser. Ctrl-C saves flash and stops.`);
});
let closing = false;
process.on('SIGINT', async () => {
  if (closing) return;
  closing = true;
  server.close();
  try { await bridge.close(); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
  finally { server.closeAllConnections(); }
});
bridge.child.on('exit', () => { server.close(); server.closeAllConnections(); });
