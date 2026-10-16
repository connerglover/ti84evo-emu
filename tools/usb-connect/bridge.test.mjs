import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startBridge } from './bridge.mjs';

async function fakeBridge(exitCode) {
  const dir = await mkdtemp(join(tmpdir(), 'evo-bridge-test-'));
  // startBridge's first argument is "usb-bridge"; Node can run a script with
  // that filename, allowing a real child/pipe test on Windows and Unix.
  await writeFile(join(dir, 'usb-bridge'), `
    const readline = require('node:readline');
    readline.createInterface({input: process.stdin}).on('line', () => {
      process.stdout.write('{"ok":true,"data":{}}\\n', () => {
        setTimeout(() => process.exit(Number(process.argv[2])), 10);
      });
    });
  `);
  const previous = process.cwd();
  let bridge;
  try { process.chdir(dir); bridge = startBridge(process.execPath, String(exitCode)); }
  finally { process.chdir(previous); }
  return { bridge, cleanup: () => rm(dir, { recursive: true, force: true }) };
}

test('close waits for the successful child exit after the quit acknowledgement', async () => {
  const { bridge, cleanup } = await fakeBridge(0);
  try { await bridge.close(); assert.equal(bridge.child.exitCode, 0); }
  finally { await cleanup(); }
});

test('a persistence failure after the quit acknowledgement rejects close', async () => {
  const { bridge, cleanup } = await fakeBridge(1);
  try { await assert.rejects(bridge.close(), /exited \(1\).*flash may not have been saved/); }
  finally { await cleanup(); }
});
