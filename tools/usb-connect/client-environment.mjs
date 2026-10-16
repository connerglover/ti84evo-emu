// Host initialization performed by the official app's GI()/KI() bootstrap, observed in
// main.4fb0b73ace793c03.js (2026-10-15). Without the first flag, the public library
// schedules every serial read timeout at zero milliseconds rather than its argument.
// Keep the communications library itself unchanged.
export function initializeClientEnvironment(target = globalThis) {
  target._0x1f6e2c = 11930994;
  target._0x4a7d90 = 387121;
  target._0x6d1e8a = 5081950;
  target._0xc8b3f5 = 3833644;
}
