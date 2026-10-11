<!--
Release notes are written on the feature branch, under the "unreleased" header at
the top. Cutting a release only replaces that header with the version (the
consumer tag, e.g. "SpritzWine 11.13-5") and starts a fresh unreleased section.
-->

# SpritzWine (unreleased)

Printing from a Windows app now looks and behaves like printing from any other
desktop app: the host's own print dialog, the host's own queues.

## New

- **Host print dialog.** `PrintDlg` and `PrintDlgEx` open the desktop's print
  dialog through the XDG print portal (`org.freedesktop.portal.Print`), the
  same mechanism sandboxed GTK apps and Firefox use. Printers, paper, copies,
  duplex and page ranges come from that dialog; the app is handed a
  "Desktop Print Dialog" printer whose new `PORTAL:` port delivers the finished
  document to the queue the user picked there. The desktop applies copies,
  collation and duplex itself; page ranges stay with the app, as they do for
  GTK apps. Works for 64-bit and 32-bit apps.
- **Never a lost job.** Apps that drive the dialog themselves (hooks, custom
  templates, property pages, the setup-only dialog) keep Wine's dialog. With
  no portal on the bus the dialog falls back to Wine's and the port delivers
  to the CUPS default queue; with neither, the spooled file is kept and
  logged. Opt out with `WINE_NO_PORTAL_PRINTDIALOG=1`.

## Known limitations

- The KDE portal backend ignores an app's wish to hide "Print to File" and
  always offers it; the file it writes is PostScript whatever name is given.
  The GTK backend honours both. The host dialog is not parented to the app
  window yet (same as the file chooser).

---

# SpritzWine 11.13-4

Bluetooth serial devices reachable from Windows apps, with no Linux-side
COM-port bridge.

## New

- **RFCOMM connections resolve their channel.** A Windows app that connects
  with `BT_PORT_ANY` and a service class now gets the channel from the
  device's SDP server, as on Windows; previously the connect failed. Needs
  `libbluetooth` at runtime, which the build image now carries.
- **Device discovery across all radios.** `BluetoothFindFirstDevice` with no
  radio handle enumerates every local adapter instead of failing, which is
  what device-scan dialogs call.

## Compatibility

- The device has to be paired once (`bluetoothctl`); unpaired links are
  dropped by firmware on first traffic.
