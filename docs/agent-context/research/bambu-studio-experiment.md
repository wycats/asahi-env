# Bambu Studio AppImage Experiment

## Objective

Test if `appimage-runner` can successfully run the Bambu Studio AppImage on Fedora Asahi Remix using `muvm` + FEX.

## Procedure

1.  Downloaded `Bambu_Studio_linux_fedora-v02.04.00.70.AppImage`.
2.  Ran with `appimage-runner`.
3.  Encountered missing library errors.

## Findings

### 1. Architecture Mismatch

The AppImage is `x86_64`. The host (and guest VM) is `aarch64`.
The AppImage is "thin" (Type 2) and expects system libraries to be present.
It looks for libraries in:

- Its own `bin` directory (via `AppRun` setting `LD_LIBRARY_PATH`).
- System paths (`/lib64`, `/usr/lib64`).

Since the system paths contain `aarch64` libraries, the x86_64 binary cannot load them.

### 2. Missing Libraries

The following libraries were missing (not bundled in the AppImage):

- `libsoup-2.4.so.1`
- `libwebkit2gtk-4.0.so.37`
- (Likely many others, including GTK3 dependencies)

### 3. Proof of Concept Fix

We manually downloaded the Fedora 42 x86_64 RPM for `libsoup`, extracted `libsoup-2.4.so.1`, and placed it in the extracted AppImage's `bin` directory.
Result: The loader successfully found `libsoup` and proceeded to fail on the next missing library (`libwebkit2gtk`).

## Conclusion

Bambu Studio cannot run "out of the box" on this setup because it relies on x86_64 system libraries that are not present in the aarch64 guest environment.

## Recommendations

To support such AppImages, we need one of the following:

1.  **FEX Rootfs Augmentation**: Install a complete x86_64 userspace (or at least the required libraries) into the FEX rootfs.
2.  **AppImage Bundling**: Use a "thicker" AppImage that bundles all dependencies (if available).
3.  **Automated Library Injection**: Extend `appimage-runner` to automatically fetch and inject missing x86_64 libraries (complex and fragile).
