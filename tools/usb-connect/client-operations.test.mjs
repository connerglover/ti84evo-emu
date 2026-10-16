import test from 'node:test';
import assert from 'node:assert/strict';
import { exitExamMode } from './client-operations.mjs';
import { CPU_HZ } from './usb-host.mjs';

test('disconnect is successful only after the guest confirms PTT cleared', async () => {
  let cycles = 0;
  const ti = {
    getDeviceInfo: async () => ({ p2t: cycles < 5 * CPU_HZ ? 'true' : 'false' }),
    sendExitPTT: async () => 11,
  };
  const result = await exitExamMode(ti, { advance: async n => { assert.ok(n <= CPU_HZ); cycles += n; } });
  assert.equal(result.wasEnabled, true);
  assert.equal(result.before.p2t, 'true');
  assert.equal(result.after.p2t, 'false');
});

test('an acknowledged request cannot conceal a failed mode transition', async () => {
  await assert.rejects(exitExamMode({
    getDeviceInfo: async () => ({ p2t: 'true' }), sendExitPTT: async () => 0,
  }, { advance: async () => {} }), /remains in exam mode/);
});

test('missing mode state and unexpected transport errors fail closed', async () => {
  await assert.rejects(exitExamMode({ getDeviceInfo: async () => ({}) }, {}), /did not report/);
  await assert.rejects(exitExamMode({
    getDeviceInfo: async () => ({ p2t: 'true' }), sendExitPTT: async () => 10,
  }, {}), /returned 10/);
});

test('already inactive mode is reported without claiming a transition', async () => {
  const result = await exitExamMode({ getDeviceInfo: async () => ({ p2t: 'false' }) }, {});
  assert.equal(result.wasEnabled, false);
});
