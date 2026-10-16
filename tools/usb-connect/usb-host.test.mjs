import test from 'node:test';
import assert from 'node:assert/strict';
import { UsbHost, EmulatedSerialPort, parseCdc } from './usb-host.mjs';

test('CDC endpoints come from the guest configuration, not fixed endpoint numbers', () => {
  const config = Uint8Array.from([
    9,2,41,0,2,1,0,128,50, 9,4,3,0,0,2,2,1,0,
    9,4,4,0,2,10,0,0,0, 7,5,0x85,2,64,0,0, 7,5,6,2,64,0,0,
  ]);
  assert.deepEqual(parseCdc(config), { controlInterface: 3,
    input: { number: 5, packetSize: 64 }, output: { number: 6, packetSize: 64 } });
  assert.throws(() => parseCdc(Uint8Array.of(0,4)), /malformed/);
  assert.throws(() => parseCdc(config.subarray(0,40)), /malformed/);
});

test('control IN retries NAK then completes the OUT status stage', async () => {
  const calls = []; let reads = 0;
  const host = new UsbHost(async r => {
    calls.push(r);
    if (r.op === 'in') {
      if (++reads === 1) throw new Error('Nak');
      return { bytes: [18,1,0,2] };
    }
    return {};
  });
  assert.deepEqual([...await host.control(0x80,6,0x100,0,18)], [18,1,0,2]);
  assert.deepEqual(calls[0], { op: 'setup', bytes: [128,6,0,1,0,0,18,0] });
  assert(calls.some(r => r.op === 'out' && r.endpoint === 0 && !r.bytes.length));
});

test('STALL is surfaced without being retried as NAK', async () => {
  let n = 0;
  const host = new UsbHost(async () => { n++; throw new Error('Stall'); });
  await assert.rejects(host.token({ op: 'in', endpoint: 1 }), /Stall/);
  assert.equal(n, 1);
});

function deferred() {
  let resolve;
  const promise = new Promise(r => { resolve = r; });
  return { promise, resolve };
}
function serialHost(overrides = {}) {
  const device = new Uint8Array(18);
  device.set([0x51, 4, 0x18, 0xe0], 8);
  return {
    enumerate: async () => ({ device, controlInterface: 0,
      input: { number: 1, packetSize: 64 }, output: { number: 2, packetSize: 64 } }),
    control: async () => {}, request: async () => ({}), advance: async () => ({}),
    ...overrides,
  };
}
const serialOptions = { baudRate: 115200, dataBits: 8, parity: 'none', stopBits: 1 };

test('reopening waits for guest detach processing', async () => {
  const calls = [], detached = deferred(), processed = deferred();
  const host = serialHost({
    request: async request => { calls.push(request.op); detached.resolve(); return {}; },
    advance: async cycles => { calls.push(cycles); await processed.promise; },
  });
  const enumerate = host.enumerate;
  host.enumerate = async () => { calls.push('enumerate'); return enumerate(); };
  const port = new EmulatedSerialPort(host);
  await port.open(serialOptions);
  const close = port.close(); await detached.promise;
  const open = port.open(serialOptions);
  await Promise.resolve();
  assert.deepEqual(calls, ['enumerate', 'attach', 39_000_000]);
  processed.resolve(); await close; await open;
  assert.equal(calls.at(-1), 'enumerate');
  await port.close();
});

test('released old reader cannot consume packets after reconnect', async () => {
  const advancing = deferred(), resume = deferred();
  let reads = 0, enumerations = 0;
  const host = serialHost({
    request: async request => {
      if (request.op !== 'in') return {};
      if (++reads === 1) throw new Error('Nak');
      return { bytes: [reads] };
    },
    advance: async () => { advancing.resolve(); return resume.promise; },
  });
  const enumerate = host.enumerate;
  host.enumerate = async () => { enumerations++; return enumerate(); };
  const port = new EmulatedSerialPort(host);
  await port.open(serialOptions);
  const oldStream = port.readable, reader = oldStream.getReader();
  const abandoned = reader.read().catch(error => error.name);
  await advancing.promise;
  // The official TI client releases its reader without cancelling an old read.
  reader.releaseLock();
  const closing = port.close(), opening = port.open(serialOptions);
  await Promise.resolve();
  assert.equal(enumerations, 1, 'reopening waits for the old poll');
  resume.resolve({ stop: 'CycleLimit' });
  await closing;
  await opening;
  assert.equal(await abandoned, 'TypeError');
  assert.deepEqual(await oldStream.getReader().read(), { done: true, value: undefined });
  const next = await port.readable.getReader().read();
  assert.deepEqual([...next.value], [2], 'first new packet reaches the new reader');
  await port.close();
});

test('closing during a write stops old packet fragments before reconnect', async () => {
  const sending = deferred(), resume = deferred();
  let writes = 0;
  const port = new EmulatedSerialPort(serialHost({
    token: async () => { writes++; sending.resolve(); await resume.promise; },
  }));
  await port.open(serialOptions);
  const writer = port.writable.getWriter();
  const written = writer.write(new Uint8Array(128)).catch(error => error.message);
  await sending.promise;
  const closing = port.close(), opening = port.open(serialOptions);
  resume.resolve();
  await closing;
  await opening;
  assert.equal(await written, 'serial port closed');
  assert.equal(writes, 1, 'second old packet never reaches the new connection');
  await port.close();
});
