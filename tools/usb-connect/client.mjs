import { UsbHost, EmulatedSerialPort, EmulatedSerial } from './usb-host.mjs';
import { initializeClientEnvironment } from './client-environment.mjs';
import { exitExamMode } from './client-operations.mjs';
initializeClientEnvironment();
const $ = id => document.getElementById(id);
const port = new EmulatedSerialPort(new UsbHost(async request => {
  const response = await fetch('/usb', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(request) });
  if (!response.ok) throw new Error(`USB bridge HTTP ${response.status}`);
  const result = await response.json();
  if (!result.ok) throw new Error(result.error);
  return result.data;
}));
Object.defineProperty(navigator, 'serial', { value: new EmulatedSerial(port), configurable: true });
let ti, busy = false, connected = false;
const status = text => { $('status').textContent = text; };
async function action(label, fn) {
  if (busy) return;
  busy = true; $('actions').disabled = true; $('cancel').disabled = !connected; status(label);
  try { await fn(); status('Ready.'); }
  catch (error) { console.error(error); status(`Failed: ${typeof error === 'number' && ti ? ti.getErrorMessage(error) : error.message || error}`); }
  finally { busy = false; $('actions').disabled = !connected; $('cancel').disabled = true; }
}
$('cancel').onclick = () => { if (busy && ti) { ti.cancelOperation(); status('Cancelling…'); } };
$('connect').onclick = () => action('Connecting…', async () => {
  $('connect').disabled = true;
  try {
    if (!ti) {
      await new Promise((resolve, reject) => {
        const script = document.createElement('script'); script.id = 'ti-connect-library';
        script.src = '/library/ticonnect.min.js';
        script.onload = resolve; script.onerror = () => reject(new Error('Could not load the official TI library'));
        document.head.appendChild(script);
      });
      ti = window.TIConnect;
    }
    await ti.init(() => {}, () => { connected = false; }, () => {}, () => {}, () => {});
    await ti.enableAutoActivate();
    if (!ti.hasActiveDevice()) throw new Error('The guest did not complete USB activation');
    connected = true; $('disconnect').disabled = false;
    $('attributes').textContent = JSON.stringify(await ti.getAttributes(), null, 2);
  } finally { $('connect').disabled = connected; }
});
$('disconnect').onclick = () => action('Disconnecting…', async () => {
  await window.TI_USB.closeActiveDevice(); connected = false;
  $('connect').disabled = false; $('disconnect').disabled = true; $('refresh').checked = false;
});
async function capture() {
  const rgba = await ti.getScreen(0);
  if (rgba.length !== 320 * 240 * 4) throw new Error('Invalid screenshot dimensions');
  $('screen').getContext('2d').putImageData(new ImageData(Uint8ClampedArray.from(rgba), 320, 240), 0, 0);
}
$('capture').onclick = () => action('Capturing screen…', capture);
setInterval(() => { if (connected && $('refresh').checked && !busy) action('Capturing screen…', capture); }, 1500);
$('directory').onclick = () => action('Reading files…', async () => {
  const files = await ti.getDirectory(); $('files').replaceChildren();
  for (const file of files) {
    const li = document.createElement('li'), button = document.createElement('button');
    button.textContent = `Download ${file.displayName || file.tokenName}`;
    button.onclick = () => action('Receiving file…', async () => {
      await ti.getFileByNameAndType(file);
      const url = URL.createObjectURL(new Blob([Uint8Array.from(file.contents)]));
      const a = document.createElement('a'); a.href = url;
      a.download = `${file.displayName || 'variable'}.${file.getFileExtension().replace(/^\./, '')}`;
      a.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
    });
    li.append(button); $('files').append(li);
  }
});
$('upload').onchange = () => action('Sending variable…', async () => {
  const file = $('upload').files[0]; if (!file) return;
  let contents = new Uint8Array(await file.arrayBuffer()), name = file.name;
  if (/\.py$/i.test(name)) {
    const python = { contents };
    const result = ti.convertPyToPyVar(python, name);
    if (result !== 0) throw new Error(ti.getErrorMessage(result));
    contents = Uint8Array.from(python.contents); name = name.replace(/\.py$/i, '.8xv2');
  }
  await ti.putFileWithBufferAndName(contents, name, false);
  $('upload').value = '';
});
$('image-upload').onchange = () => action('Sending image…', async () => {
  const file = $('image-upload').files[0]; if (!file) return;
  const bitmap = await createImageBitmap(file);
  const canvas = document.createElement('canvas'); canvas.width = bitmap.width; canvas.height = bitmap.height;
  canvas.getContext('2d').drawImage(bitmap, 0, 0); bitmap.close();
  const slot = Number($('image-slot').value);
  const contents = await ti.convertPNGtoImageVar(canvas.toDataURL('image/png'), 1);
  const image = new window.TIFile({ dispName: `Image${slot}`,
    tokName: `E8${(0xb0 + (slot === 0 ? 9 : slot - 1)).toString(16).toUpperCase()}`,
    type: window.TIFile.fileTypeFromExtension('.8ca2'), size: contents.length,
    memory: 'Archive', contents: Uint8Array.from(contents) });
  await ti.sendFile(image, true);
  $('image-upload').value = '';
});
$('exit-ptt').onclick = () => action('Exiting exam mode…', async () => {
  await exitExamMode(ti, port.host);
});
$('os').onchange = () => action('Installing OS…', async () => {
  const file = $('os').files[0]; if (!file) return;
  const result = await ti.sendBundle(new Uint8Array(await file.arrayBuffer()));
  if (result !== 0) throw new Error(`TI Connect returned ${result}`);
  $('os').value = '';
});
