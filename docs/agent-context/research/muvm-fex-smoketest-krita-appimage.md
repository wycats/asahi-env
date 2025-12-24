# muvm + FEX smoketest: Krita (x86_64 AppImage)

Date: 2025-12-20

## Goal

Run a **complex x86_64 GUI app** under `muvm` using `--emu=fex`, as a sanity/control test that is less "Chromium-y" than Edge.

## Result

✅ Krita window appeared and was usable.

## Key gotcha: AppImage wants FUSE

Running the AppImage directly inside `muvm` fails because the guest does not have working FUSE (`fusermount` missing and `/dev/fuse` permission denied).

Workaround: extract the AppImage on the host without executing it, then run the extracted `AppRun` under `muvm`.

## Download

- Source: https://download.kde.org/stable/krita/5.2.13/krita-5.2.13-x86_64.AppImage

Stored at:

- `.local/apps/krita-5.2.13-x86_64.AppImage`

## Host-side extraction (no FUSE)

The embedded SquashFS superblock is not at the first `hsqs` occurrence; locate a _plausible_ SquashFS superblock (major=4, sane block size) and use that offset.

In this run, the correct offset was `944632`.

Commands:

- `unsquashfs -q -o 944632 -d .local/apps/krita-5.2.13-host-extract/squashfs-root .local/apps/krita-5.2.13-x86_64.AppImage`

Extracted tree:

- `.local/apps/krita-5.2.13-host-extract/squashfs-root/`

## Launch under muvm+FEX

Initial launch without extra env failed with embedded-Python error (`No module named 'encodings'`) because Krita wasn’t finding its bundled stdlib.

Fix: set `PYTHONHOME` to the extracted bundle’s `usr`.

Command:

- `muvm --emu=fex -e PYTHONHOME=/home/wycats/Code/Personal/asahi/.local/apps/krita-5.2.13-host-extract/squashfs-root/usr /home/wycats/Code/Personal/asahi/.local/apps/krita-5.2.13-host-extract/squashfs-root/AppRun`

## Notes / warnings observed

- `QStandardPaths: wrong permissions on runtime directory /tmp/muvm-run-… (0755 instead of 0700)`
- `Fontconfig error: Cannot load default config file: (null)`
- These warnings did not prevent Krita from launching after setting `PYTHONHOME`.

## Why this matters

This is an existence proof that:

- `muvm` can run a substantial **x86_64 GUI** workload under FEX,
- even when Chromium/Edge workloads are unstable.
