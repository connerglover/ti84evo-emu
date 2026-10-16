import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const serve = fileURLToPath(new URL('./serve.mjs', import.meta.url));
async function fixture() {
  const dir = await mkdtemp(join(tmpdir(), 'evo-serve-test-'));
  for (const name of ['ticonnect.min.js', 'ticonnect.js', 'ticonnect.worker.js']) {
    await writeFile(join(dir, name), '');
  }
  await writeFile(join(dir, 'usb-bridge'), `
    const fs = require('node:fs');
    fs.writeFileSync('started', 'yes');
    require('node:readline').createInterface({input: process.stdin})
      .on('line', line => {
        if (JSON.parse(line).op === 'quit') {
          process.stdout.write('{"ok":true,"data":{}}\\n', () => {
            fs.writeFileSync('persisted', 'yes'); process.exit(0);
          });
        }
      }).on('close', () => process.exit(0));
  `);
  return dir;
}
async function launch(dir, extra = []) {
  const child = spawn(process.execPath, [serve, '--cli', process.execPath,
    '--bundle', 'unused', '--library', dir, ...extra], { cwd: dir, windowsHide: true });
  let output = '';
  child.stdout.on('data', data => { output += data; });
  child.stderr.on('data', data => { output += data; });
  const timer = setTimeout(() => child.kill(), 5000);
  try {
    const [code, signal] = await once(child, 'close');
    assert.equal(signal, null, `server did not shut down: ${output}`);
    return { code, output };
  } finally { clearTimeout(timer); }
}

test('occupied HTTP port shuts down and persists the already-spawned guest', async () => {
  const dir = await fixture(), occupied = createServer();
  occupied.listen(0, '127.0.0.1');
  await once(occupied, 'listening');
  try {
    const result = await launch(dir, ['--port', String(occupied.address().port)]);
    assert.equal(result.code, 1);
    assert.match(result.output, /EADDRINUSE/);
    assert.equal(await readFile(join(dir, 'persisted'), 'utf8'), 'yes');
  } finally { occupied.close(); await rm(dir, { recursive: true, force: true }); }
});

test('missing CLI closes the HTTP server and returns failure', async () => {
  const dir = await fixture();
  try {
    const result = await launch(dir, ['--cli', join(dir, 'missing-cli.exe')]);
    assert.equal(result.code, 1);
    assert.match(result.output, /USB bridge stopped/);
  } finally { await rm(dir, { recursive: true, force: true }); }
});

test('invalid port and missing library fail before starting a guest', async () => {
  const dir = await fixture();
  try {
    for (const extra of [['--port', '-1'], ['--library', join(dir, 'missing')]]) {
      const result = await launch(dir, extra);
      assert.equal(result.code, 1);
      await assert.rejects(readFile(join(dir, 'started')), { code: 'ENOENT' });
    }
  } finally { await rm(dir, { recursive: true, force: true }); }
});
