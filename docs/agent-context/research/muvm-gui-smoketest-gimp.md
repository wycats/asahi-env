# muvm GUI smoketest: GIMP

Date: 2025-12-20

## Goal
Validate that `muvm` can run a non-trivial GUI app via sommelier/Wayland forwarding.

## Setup
- Installed on host:
  - `sommelier`
  - `gimp`

## Command
- `muvm gimp`

## Result
- GIMP window appeared and was usable (created a new image / canvas).
- Terminal output included warnings about the accessibility bus (`/run/user/1000/at-spi/bus` missing) and a udev rules permission warning; these did not prevent the app from launching.

## Notes
- This is a **native (aarch64) workload**, so it does not exercise FEX emulation.
- Useful as a control: it suggests the VM boot + GUI forwarding path can work reliably even when Chromium/FEX workloads fail.
