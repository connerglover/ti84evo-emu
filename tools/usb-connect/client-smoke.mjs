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
import { exitExamMode } from './client-operations.mjs';

const { values } = parseArgs({ options: {
  cli: { type: 'string' }, bundle: { type: 'string' }, library: { type: 'string' },
  flash: { type: 'string' }, provision: { type: 'boolean' }, output: { type: 'string' },
  trace: { type: 'boolean' },
  roundtrip: { type: 'boolean' }, 'exit-ptt': { type: 'boolean' },
  'install-bundle': { type: 'string' },
  'python-roundtrip': { type: 'boolean' },
  'skip-screenshots': { type: 'boolean' },
  'receive-ptt': { type: 'boolean' },
  'recovery-checks': { type: 'boolean' },
  keys: { type: 'string' },
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
  for (const name of (values.keys || '').split(/\s+/).filter(Boolean)) {
    await bridge.request({ op: 'key', name, pressed: true });
    await bridge.request({ op: 'run', cycles: 7_800_000 });
    await bridge.request({ op: 'key', name, pressed: false });
    await bridge.request({ op: 'run', cycles: 78_000_000 });
  }
  for (const name of ['ticonnect.min.js', 'ticonnect.js']) {
    const source = await readFile(join(values.library, name), 'utf8');
    vm.runInContext(source, context, { filename: name, timeout: 10_000 });
  }
  const ti = context.TIConnect;
  let activationError;
  let cancelOnProgress = false, cancelRequested = false;
  await ti.init(() => {}, () => {}, (_event, bytes) => {
    if (cancelOnProgress && bytes > 0) {
      cancelOnProgress = false; cancelRequested = true;
      ti.cancelOperation();
    }
  }, () => {}, () => { activationError = true; });
  await ti.enableAutoActivate();
  if (activationError || !ti.hasActiveDevice()) throw new Error('TI Connect did not activate the emulated calculator');
  const attributes = await ti.getAttributes();
  console.log('ATTRIBUTES_BEFORE', JSON.stringify(attributes));
  if (values['exit-ptt']) {
    const transition = await exitExamMode(ti, port.host);
    if (!transition.wasEnabled) throw new Error('Exam-exit verification requires active exam mode');
    console.log('EXIT_PTT', JSON.stringify(transition));
  }
  const receiveBefore = values['receive-ptt'] ? await ti.getDeviceInfo() : undefined;
  if (receiveBefore && receiveBefore.p2t !== 'true') throw new Error('Variable exam-exit verification requires active exam mode');
  const directory = await ti.getDirectory();
  let screen;
  // Exercise exam exit independently of optional screenshot restrictions.
  if (!values['skip-screenshots']) {
    screen = Uint8Array.from(await ti.getScreen(0));
    if (screen.length !== 320 * 240 * 4) throw new Error(`unexpected screenshot size ${screen.length}`);
    // A second request catches stale FIFO state and transfer completion bugs.
    const screen2 = await ti.getScreen(0);
    if (screen2.length !== screen.length) throw new Error('second screenshot failed');
  }
  if (values.roundtrip || values['receive-ptt']) {
    const file = directory.find(f => f.displayName === 'L1');
    if (!file) throw new Error('round-trip test requires the standard L1 list');
    await ti.getFileByNameAndType(file);
    const original = Uint8Array.from(file.contents);
    // Write back the exact received variable, with overwrite explicitly selected.
    file.contents = original;
    await ti.sendFile(file, true);
    if (values['receive-ptt']) {
      for (let i = 0; i < 5; i++) await port.host.advance(156_000_000);
      const after = await ti.getDeviceInfo();
      if (after.p2t !== 'false') throw new Error('Calculator remains in exam mode after receiving a variable');
      console.log('RECEIVE_PTT', JSON.stringify({ before: receiveBefore, after }));
    }
    await ti.getFileByNameAndType(file);
    const received = Uint8Array.from(file.contents);
    if (!Buffer.from(received).equals(Buffer.from(original))) throw new Error('variable round-trip differs');
    console.log(`ROUNDTRIP L1: ${original.length} bytes identical`);
  }
  if (values['python-roundtrip']) {
    const name = 'ZUSBTST';
    if (directory.some(f => f.displayName === name)) throw new Error(`${name} already exists; use a disposable flash copy`);
    const source = new TextEncoder().encode('print(6 * 7)\n');
    const file = { contents: source };
    const result = ti.convertPyToPyVar(file, `${name}.py`);
    if (result !== 0) throw new Error(`Python conversion returned ${result}`);
    const converted = Uint8Array.from(file.contents);
    await ti.putFileWithBufferAndName(converted, `${name}.8xv2`, false);
    const received = (await ti.getDirectory()).find(f => f.displayName === name);
    if (!received) throw new Error('uploaded Python variable is absent from directory');
    await ti.getFileByNameAndType(received);
    // The official receive API converts a Python app variable back to .py text.
    if (!Buffer.from(received.contents).equals(Buffer.from(source))) throw new Error('Python source round-trip differs');
    await ti.deleteFile(received);
    if ((await ti.getDirectory()).some(f => f.displayName === name)) throw new Error('Python variable delete failed');
    console.log(`PYTHON_ROUNDTRIP: ${source.length} source bytes identical; deleted test variable`);
  }
  if (values['recovery-checks']) {
    const file = directory.find(f => f.displayName === 'L1');
    if (!file) throw new Error('recovery test requires L1');
    await ti.getFileByNameAndType(file);
    const original = Uint8Array.from(file.contents);
    file.contents = Uint8Array.of(0);
    let rejection;
    try { await ti.sendFile(file, true); }
    catch (error) { rejection = error; }
    // This firmware closes the malformed CBOR upload with Kermit DI. A timeout
    // would not prove rejection; require the observed response and intact data.
    if (rejection !== 11) throw new Error(`Malformed variable returned ${rejection}, expected disconnect 11`);
    console.log('REJECTED_VARIABLE', rejection);
    await ti.getFileByNameAndType(file);
    if (!Buffer.from(file.contents).equals(Buffer.from(original))) throw new Error('Rejected upload changed L1');
    cancelOnProgress = true;
    let cancellation;
    try { await ti.getScreen(0); }
    catch (error) { cancellation = error; }
    finally { cancelOnProgress = false; }
    if (!cancelRequested || cancellation !== 9) throw new Error(`Screenshot cancellation returned ${cancellation}`);
    await ti.getDirectory();
    const recovered = await ti.getScreen(0);
    if (recovered.length !== 320 * 240 * 4) throw new Error('Screenshot after cancellation failed');
    console.log('RECOVERY_CHECKS: rejection preserved L1; cancellation followed by directory and screenshot succeeded');
  }
  if (values['install-bundle']) {
    const update = new Uint8Array(await readFile(values['install-bundle']));
    console.log(`INSTALL_BUNDLE: sending ${update.length} bytes`);
    const result = await ti.sendBundle(update);
    if (result !== 0) throw new Error(`OS installation returned ${result}`);
    console.log('INSTALL_BUNDLE: success');
  }
  if (values.output && screen) await writeFile(values.output, screen);
  console.log(JSON.stringify({ attributes, variables: directory.map(f => ({ name: f.displayName, type: f.type })), screenshotBytes: screen?.length }, null, 2));
} finally {
  try { if (port.opened) await port.close(); }
  finally {
    try {
      context.PThread?.terminateAllThreads();
      for (const timer of timers) clearTimeout(timer);
    } finally { await bridge.close(); }
  }
}
