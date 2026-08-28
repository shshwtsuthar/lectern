# Kobo USB device support

Lectern supports Kobo e-ink readers that expose their normal mounted USB mass-storage filesystem.
This first device tranche covers connection, storage inspection, EPUB/PDF transfer, device-file
listing, safe removal of Lectern-managed copies, and operating-system eject. It does not implement
Kobo Store access, DRM, account authentication, cloud sync, KEPUB conversion, collections, cover
synchronization, or reading-progress synchronization.

## Architecture and safety boundaries

`lectern-device` is a desktop adapter with generic reader, mounted-volume, format-priority, and
operation types. `DeviceManager` maintains a map of stable device IDs to per-device sessions and
dispatches through an injectable `RemovableStorageProvider`. Kobo recognition is one capability
driver rather than application-wide special cases, so another mounted-storage reader can add a
driver without replacing transfer or UI state.

A Kobo must have a real, non-symlink `.kobo/` directory. A `KOBOeReader` label alone is not enough.
Identity prefers the validated serial field in `.kobo/version`, then an operating-system volume
identifier, then a stable hash of volume properties; the transient mount path is never the sole
identity. Lectern displays `Kobo eReader` unless the mounted volume provides a more specific name.
It does not guess a model.

Transfers use deterministic EPUB-before-PDF priority and write only below
`Books/<sanitized author>/<sanitized title>.<format>`. Metadata and device paths are untrusted:
portable component sanitization, bounded UTF-8 names, root confinement, parent-chain checks, and
symlink rejection apply before writes or deletion. Copying uses a 256 KiB reusable buffer, a hidden
partial file, flush plus `sync_all`, and final rename. Cancellation or disconnection removes the
known partial file and never changes the library source.

`device-transfers.json`, stored beside the library database, records device ID, book and asset IDs,
relative path, source size, SHA-256, and transfer time. This history accelerates reconnect matching
but is not authoritative: the device filesystem is inspected again, and deletion re-hashes the
target before removing it. Lectern only offers removal for a history-owned file inside `Books/`;
manual sideloads and all `.kobo/` content are read-only. `KoboReader.sqlite` is neither read nor
written.

## Desktop behavior

The GPUI application reconciles mounted volumes on a background two-second cadence. Detection work
enumerates the operating system's mounted-volume list, then checks `.kobo` only on removable or
platform-typical mount roots. Connecting or unplugging updates the top bar and status area without
restarting Lectern.

Select one or more library books and choose **Send to Kobo**. With multiple readers connected,
Lectern asks for the target once for the batch. Planning revalidates library selection generation,
source existence, compatible formats, destination collisions, and free space before copying.
Previous unchanged Lectern transfers can be replaced with one batch decision; identical files are
recognized and unrelated collisions are skipped. The UI reports current title, item count, bytes,
percentage, throughput, failures, and cancellation state while the background transfer runs.

Choose the generic reader button in the top bar to view storage and files under `Books/`. Entries
show whether local transfer history correlates them to a library book. **Remove** deletes only the
verified device copy. **Eject Kobo** prevents a same-device operation race and reports “safe to
disconnect” only after the operating system confirms that the mount disappeared.

## Platform behavior

- Linux discovers mounted volumes through the native system inventory and requests unmount plus
  power-off with argument-safe `udisksctl` calls. Eject requires a `/dev/...` block-device source
  and a working udisks service.
- macOS uses the mounted-volume inventory and validated `diskutil info/eject` arguments below
  `/Volumes`.
- Windows uses the mounted-volume inventory and validated drive-root `mountvol /L` and `/P`
  operations.

The platform boundary is mockable; CI does not require real hardware. A platform command failure or
missing facility is surfaced as the eject error, and Lectern never claims ejection succeeded merely
because the UI hid a device.

## Performance checks

The device workload measures the production mounted-volume provider, then uses 32 deterministic
candidate volumes and a 120-book, 1 MiB-per-book release transfer. Run:

```sh
python3 benchmarks/kobo_device_regression.py \
  --budget benchmarks/kobo-device-regression-v1.json \
  --output-dir target/benchmarks/kobo-device-local
```

The GPUI workload renders one connected reader with 128 device books and retains every raw sample:

```sh
python3 benchmarks/kobo_device_ui_regression.py \
  --budget benchmarks/kobo-device-ui-regression-v1.json \
  --output-dir target/benchmarks/kobo-device-ui-local
```

## Manual test checklist

Use expendable device content for interruption tests and keep the Kobo charged.

1. Launch Lectern without a Kobo and confirm no device control is shown.
2. Connect a Kobo over USB, choose **Connect** on the reader, and confirm automatic detection.
3. Confirm the reader name and free/total storage are plausible.
4. Send one EPUB and confirm progress completes without blocking the application.
5. Send one PDF and confirm its device-relative destination is below `Books/`.
6. Send several selected books and confirm item and byte progress advance.
7. Send an unchanged book again and confirm it is recognized rather than silently overwritten.
8. Change a previously transferred source, retry, and exercise both **Skip existing** and
   **Replace previous transfers**.
9. Open the device view, remove a Lectern-managed copy, and confirm the library copy remains.
10. Choose **Eject Kobo**, wait for the success message, and only then disconnect the cable.
11. Confirm Kobo firmware imports the EPUB and PDF after disconnect.
12. Reconnect and confirm previously transferred files correlate with the library.
13. Unplug while idle and confirm the device UI disappears without a restart or crash.
14. With a disposable large source, cancel during transfer and confirm no `.part` file remains.
15. With disposable device state, unplug during transfer and confirm failure is reported, the
    original library file remains, and the interrupted destination is not marked successful.

Hardware validation is still required across representative Linux, macOS, and Windows hosts.
Exact model names remain unavailable when a Kobo exposes no reliable model metadata, and Lectern
currently manages only its controlled internal-storage `Books/` tree; SD-card storage and arbitrary
pre-existing sideload directories are future work.
