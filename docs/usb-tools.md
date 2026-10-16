# Desktop USB tools

Open **Files → USB tools** from the left toolbar. The window follows the desktop
app's theme and works with the calculator already open. It uses native USB CDC and
Kermit transfers through the guest firmware, without a browser or JavaScript runtime.
The demonstration calculator has no USB device; load an OS bundle first.

## Files

- **Send files…** accepts Evo variables, Python `.py`, PNG pictures, and numeric CSV.
  Review the calculator names and destination before pressing **Send to calculator**.
  Multiple files can be selected in the native file chooser. Rename individual files
  in the review list; leave a name blank to keep its existing name.
- **Store in Archive** is selected by default and keeps files across emulator restarts.
  RAM files are cleared by Reset and are not saved when the emulator closes. Some file
  types require a fixed location; calculator images always use Archive.
  **Replace existing files** is off by default.
- PNG files fit inside 160×105 pixels, centered on white, and go into Image1–Image10.
  CSV can become up to six lists (999 rows each) or a matrix (at most 400 cells).
  List names can be comma-separated; blank uses L1, L2, and so on. A matrix defaults
  to A. Invalid files are rejected before the USB connection starts.
- Select files and choose **Save selected…** to receive them into a folder. Python
  programs are exported as source; other variables retain their native Evo format.
  Existing computer files are preserved by adding a numbered suffix.
- **Delete…** asks you to confirm the selection inside the tool. Refresh rereads the
  calculator directory. A partial batch preserves completed downloads and lists the
  failed file; the remaining files are not silently treated as successful.

## Screen and Calculator

**Screen** captures through the calculator's USB screen service. Copy the result to
the clipboard, save a PNG, or enable automatic refresh. Refresh runs while the Screen
tab is visible and stops after an error. Exam mode can restrict screenshots.

**Calculator** shows device/OS details, free memory, and exam status. **Exit exam mode**
sends the normal Press-to-Test exit request and verifies that the OS reports it off.

For OS updates, choose a local `.84b2` or use **Check latest OS**, then **Download**.
The catalog and download come directly from TI's public server. Downloading does not
install anything: review the bundle and press **Install OS**. The updater validates
the bundle, sends its package over USB, and reports when to reset the calculator.
Save a flash backup from Files before installing a different OS version.

## Developer

The Developer tab exposes USB resource reads and opt-in raw writes, diagnostic log
downloads, debug messages, and a bounded session activity log. Responses can be saved
without conversion; the on-screen binary/text preview is limited to 16 KiB. Progress
and packet summaries are sampled, with the last 200 entries retained. Resource writes
can change calculator data. These diagnostics do not bypass the guest firmware.

## Execution and limits

A worker temporarily owns the same running calculator during each USB operation;
the desktop remains responsive and retains the last calculator display. The worker
advances guest time even when normal emulation is paused, then restores the prior
pause/run intent. Reset, calculator replacement and state-loading actions wait for
the transfer. Debugger breakpoints/watchpoints remain effective and can stop a job.

**Cancel transfer** stops cooperatively and disconnects the virtual cable. Completed
writes can already have changed the calculator, so its nonvolatile state is saved on
success, error and cancellation. Closing the app cancels and joins the worker before
the final save. No second calculator instance is created.

The menu covers the observed TI Connect Evo workflows, plus developer diagnostics.
It is an independent implementation, not TI's application. Real-firmware validation
currently uses OS 7.1.0.4421; a same-version OS transfer has been exercised, while an
upgrade between different versions remains unverified. This is not a Windows virtual
USB driver, and host/accessory emulation and full bootloader recovery are separate
work. See the [parity inventory](usb-connect-parity.md),
[protocol/format notes](hardware/connect-file-formats.md), and
[USB compatibility evidence](hardware/link-compatibility.md).
