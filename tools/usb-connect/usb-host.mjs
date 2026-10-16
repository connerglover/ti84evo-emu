// USB host and CDC ACM serial adapter. No calculator commands are synthesized here.
export const CPU_HZ = 156_000_000;

export class UsbHost {
  constructor(request) { this.request = request; this.controlPacketSize = 64; }
  advance(cycles = 156_000) { return this.request({ op: 'run', cycles }); }
  async token(request, timeoutCycles = CPU_HZ) {
    for (let elapsed = 0; elapsed <= timeoutCycles; elapsed += 156_000) {
      try { return await this.request(request); }
      catch (error) { if (error.message !== 'Nak') throw error; }
      await this.advance();
    }
    throw new Error(`USB ${request.op} endpoint ${request.endpoint} timed out`);
  }
  async control(type, request, value, index, lengthOrBytes = 0) {
    const input = Boolean(type & 0x80);
    const payload = input ? [] : Array.from(lengthOrBytes || []);
    const length = input ? lengthOrBytes : payload.length;
    await this.request({ op: 'setup', bytes: [type, request, value & 255, value >> 8,
      index & 255, index >> 8, length & 255, length >> 8] });
    await this.advance();
    const result = [];
    if (input) {
      while (result.length < length) {
        const { bytes } = await this.token({ op: 'in', endpoint: 0 });
        result.push(...bytes);
        if (bytes.length < this.controlPacketSize) break;
      }
      await this.token({ op: 'out', endpoint: 0, bytes: [] });
    } else {
      for (let p = 0; p < payload.length; p += this.controlPacketSize) {
        await this.token({ op: 'out', endpoint: 0, bytes: payload.slice(p, p + this.controlPacketSize) });
        await this.advance();
      }
      const status = await this.token({ op: 'in', endpoint: 0 });
      if (status.bytes.length) throw new Error('invalid control status packet');
    }
    await this.advance();
    return Uint8Array.from(result.slice(0, length));
  }
  async enumerate() {
    await this.request({ op: 'attach', attached: true });
    await this.advance(CPU_HZ / 4);
    await this.request({ op: 'reset' });
    await this.advance(CPU_HZ / 100);
    const device = await this.control(0x80, 6, 0x100, 0, 18);
    if (device.length !== 18 || device[1] !== 1) throw new Error('invalid device descriptor');
    this.controlPacketSize = device[7];
    if (![8, 16, 32, 64].includes(device[7])) throw new Error('invalid EP0 packet size');
    await this.control(0, 5, 1, 0);
    const header = await this.control(0x80, 6, 0x200, 0, 9);
    if (header.length !== 9) throw new Error('invalid configuration header');
    const config = await this.control(0x80, 6, 0x200, 0, header[2] | header[3] << 8);
    const cdc = parseCdc(config);
    await this.control(0, 9, config[5], 0);
    return { device, config, ...cdc };
  }
}

export function parseCdc(config) {
  let interfaceClass, controlInterface, input, output;
  for (let p = 0; p < config.length;) {
    const n = config[p];
    if (n < 2 || p + n > config.length) throw new Error('malformed configuration descriptor');
    if (config[p + 1] === 4 && n >= 9) {
      interfaceClass = config[p + 5];
      if (interfaceClass === 2) controlInterface = config[p + 2];
    }
    if (config[p + 1] === 5 && n >= 7 && interfaceClass === 10 && (config[p + 3] & 3) === 2) {
      const ep = { number: config[p + 2] & 15, packetSize: (config[p + 4] | config[p + 5] << 8) & 0x7ff };
      if (!ep.number || !ep.packetSize || ep.packetSize > 512) throw new Error('invalid CDC endpoint');
      if (config[p + 2] & 0x80) input = ep; else output = ep;
    }
    p += n;
  }
  if (controlInterface === undefined || !input || !output) throw new Error('CDC ACM interfaces not found');
  return { controlInterface, input, output };
}

export class EmulatedSerialPort extends EventTarget {
  constructor(host) { super(); this.host = host; this.productName = 'TI-84 Evo emulator'; }
  getInfo() { return { usbVendorId: 0x0451, usbProductId: 0xe018 }; }
  async open(options) {
    if (this.closing) await this.closing;
    if (this.opened) throw new Error('serial port already open');
    if (options.baudRate !== 115200 || options.dataBits !== 8 || options.parity !== 'none' || options.stopBits !== 1) {
      throw new Error('TI Connect requires 115200 8N1');
    }
    this.cdc = await this.host.enumerate();
    const d = this.cdc.device;
    if ((d[8] | d[9] << 8) !== 0x0451 || (d[10] | d[11] << 8) !== 0xe018) {
      throw new Error('guest did not enumerate as a TI-84 Evo');
    }
    await this.host.control(0x21, 0x20, 0, this.cdc.controlInterface, [0, 0xc2, 1, 0, 0, 0, 8]);
    await this.host.control(0x21, 0x22, 3, this.cdc.controlInterface);
    this.opened = true;
    const cdc = this.cdc;
    const session = this.session = { active: true, cancelled: false, pull: null, write: null };
    this.readable = new ReadableStream({
      pull: controller => session.pull = (async () => {
        while (session.active && !session.cancelled) {
          try {
            const { bytes } = await this.host.request({ op: 'in', endpoint: cdc.input.number });
            if (!session.active || session.cancelled) break;
            if (bytes.length) { controller.enqueue(Uint8Array.from(bytes)); return; }
          } catch (error) { if (error.message !== 'Nak') throw error; }
          if (!session.active || session.cancelled) break;
          // The RPC already yields. A 1 ms timer can sleep an entire Windows timer tick,
          // starving the guest while TI's client counts its 100 ms receive timeout.
          const progress = await this.host.advance(780_000);
          if (progress.stop === 'Idle') await new Promise(resolve => setTimeout(resolve, 5));
        }
        if (!session.cancelled) controller.close();
      })(),
      cancel: () => { session.cancelled = true; return session.pull?.catch(() => {}); },
    }, { highWaterMark: 0 });
    this.writable = new WritableStream({
      write: data => session.write = (async () => {
        for (let p = 0; p < data.length; p += cdc.output.packetSize) {
          if (!session.active) throw new Error('serial port closed');
          await this.host.token({ op: 'out', endpoint: cdc.output.number,
            bytes: Array.from(data.subarray(p, p + cdc.output.packetSize)) });
          if (!session.active) throw new Error('serial port closed');
          await this.host.advance();
        }
      })(),
    });
  }
  async close() {
    if (this.closing) return this.closing;
    this.opened = false;
    const session = this.session;
    if (session) session.active = false;
    this.closing = (async () => {
      // TI releases reader locks without cancelling pending reads. Drain the old
      // session before a new enumeration can give those reads fresh packets.
      await Promise.allSettled([session?.pull, session?.write]);
      try {
        await this.host.request({ op: 'attach', attached: false });
        // Deliver the detach IRQ and let the guest tear down its CDC session.
        // Reattaching before any guest time passes can merge both CC edges.
        await this.host.advance(CPU_HZ / 4);
      }
      finally { this.readable = this.writable = null; this.session = null; }
    })();
    try { await this.closing; }
    finally { this.closing = null; }
  }
  async setSignals({ dataTerminalReady = true, requestToSend = true } = {}) {
    await this.host.control(0x21, 0x22, Number(dataTerminalReady) | Number(requestToSend) << 1, this.cdc.controlInterface);
  }
}

export class EmulatedSerial extends EventTarget {
  constructor(port) { super(); this.port = port; }
  async getPorts() { return [this.port]; }
  async requestPort() { return this.port; }
}
