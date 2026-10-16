import { CPU_HZ } from './usb-host.mjs';

function examEnabled(info) {
  if (info?.p2t !== 'true' && info?.p2t !== 'false') {
    throw new Error('Calculator did not report its exam-mode state');
  }
  return info.p2t === 'true';
}

// The real guest acknowledges the request, sends Kermit DI (client result 11),
// then clears PTT asynchronously. Neither that disconnect nor result 0 alone
// proves success: wait for guest work and query the official dynamic-info API.
export async function exitExamMode(ti, host) {
  const before = await ti.getDeviceInfo();
  if (!examEnabled(before)) return { wasEnabled: false, before, after: before };
  const result = await ti.sendExitPTT();
  if (result !== 0 && result !== 11) throw new Error(`Exit exam mode returned ${result}`);
  for (let second = 0; second < 5; second++) await host.advance(CPU_HZ);
  const after = await ti.getDeviceInfo();
  if (examEnabled(after)) throw new Error('Calculator remains in exam mode after the exit request');
  return { wasEnabled: true, before, after, result };
}
