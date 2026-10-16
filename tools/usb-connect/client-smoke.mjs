// Run the unmodified TI Connect communications library against the emulated USB bus.
// Supply locally downloaded public scripts; TI code is not distributed by this project.
import { readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { createRequire } from 'node:module';
import vm from 'node:vm';
import { parseArgs } from 'node:util';
import { startBridge } from './bridge.mjs';
import { UsbHost, EmulatedSerialPort, EmulatedSerial } from './usb-host.mjs';
import { initializeClientEnvironment } from './client-environment.mjs';

const { values } = parseArgs({ options: {
  cli: { type: 'string' }, bundle: { type: 'string' }, library: { type: 'string' },
  flash: { type: 'string' }, provision: { type: 'boolean' }, output: { type: 'string' },
  trace: { type: 'boolean' },
  roundtrip: { type: 'boolean' }, 'exit-ptt': { type: 'boolean' },
} });
for (const name of ['cli', 'bundle', 'library']) if (!values[name]) throw new Error(`--${name} is required`);
const bridge = startBridge(values.cli, values.bundle, { flashFile: values.flash, provision: values.provision });
const port = new EmulatedSerialPort(new UsbHost(async req => {
  if (values.trace && req.op === 'out' && req.endpoint) console.error('USB OUT', performance.now(), Buffer.from(req.bytes).toString('hex'));
  const result = await bridge.request(req);
  if (values.trace && req.op === 'in' && req.endpoint) console.error('USB IN', performance.now(), Buffer.from(result.bytes).toString('hex'));
  return result;
}));
const timers = new Set();
const sandbox = { console, TextDecoder, TextEncoder, Uint8Array, ArrayBuffer,
  process, require: createRequire(import.meta.url),
  __filename: resolve(values.library, 'ticonnect.js'), __dirname: resolve(values.library),
  performance, navigator: { serial: new EmulatedSerial(port) },
  setTimeout: (fn, delay, ...args) => {
    const timer = setTimeout(() => { timers.delete(timer); fn(...args); }, delay);
    timers.add(timer); return timer;
  }, clearTimeout: timer => { timers.delete(timer); clearTimeout(timer); },
  document: { currentScript: { id: 'ticonnect', src: 'https://connectevo.ti.com/ticevo/en/assets/library/ticonnect.min.js' },
    getElementById: () => null, createElement: () => ({}), head: { appendChild: script => queueMicrotask(() => script.onload()) } },
};
sandbox.window = sandbox;
sandbox.self = sandbox;
sandbox.global = sandbox;
initializeClientEnvironment(sandbox);
const context = vm.createContext(sandbox);
try {
  for (const name of ['ticonnect.min.js', 'ticonnect.js']) {
    const source = await readFile(join(values.library, name), 'utf8');
    vm.runInContext(source, context, { filename: name, timeout: 10_000 });
  }
  const ti = context.TIConnect;
  let activationError;
  await ti.init(() => {}, () => {}, () => {}, () => {}, () => { activationError = true; });
  await ti.enableAutoActivate();
  if (activationError || !ti.hasActiveDevice()) throw new Error('TI Connect did not activate the emulated calculator');
  const attributes = await ti.getAttributes();
  const directory = await ti.getDirectory();
  const screen = Uint8Array.from(await ti.getScreen(0));
  if (screen.length !== 320 * 240 * 4) throw new Error(`unexpected screenshot size ${screen.length}`);
  // A second request catches stale FIFO state and transfer completion bugs.
  const screen2 = await ti.getScreen(0);
  if (screen2.length !== screen.length) throw new Error('second screenshot failed');
  if (values.roundtrip) {
    const file = directory.find(f => f.displayName === 'L1');
    if (!file) throw new Error('round-trip test requires the standard L1 list');
    await ti.getFileByNameAndType(file);
    const original = Uint8Array.from(file.contents);
    // Write back the exact received variable, with overwrite explicitly selected.
    file.contents = original;
    await ti.sendFile(file, true);
    await ti.getFileByNameAndType(file);
    const received = Uint8Array.from(file.contents);
    if (!Buffer.from(received).equals(Buffer.from(original))) throw new Error('variable round-trip differs');
    console.log(`ROUNDTRIP L1: ${original.length} bytes identical`);
  }
  if (values['exit-ptt']) {
    const result = await ti.sendExitPTT();
    if (result !== 0) throw new Error(`exit exam mode returned ${result}`);
    console.log('EXIT_PTT: success');
  }
  if (values.output) await writeFile(values.output, screen);
  console.log(JSON.stringify({ attributes, variables: directory.map(f => ({ name: f.displayName, type: f.type })), screenshotBytes: screen.length }, null, 2));
} finally {
  try { if (port.opened) await port.close(); }
  finally {
    try {
      context.PThread?.terminateAllThreads();
      for (const timer of timers) clearTimeout(timer);
    } finally { await bridge.close(); }
  }
}
