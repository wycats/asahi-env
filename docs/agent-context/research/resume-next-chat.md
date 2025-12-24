# Resume checklist (next chat)

## Where we left off

- Edge under muvm+FEX remains unstable; evidence points to renderer SIGTRAP inside FEXInterpreter mappings.
- A clean muvm patch was prepared locally to raise vm.max_map_count in guest init; not submitted.
- Two control experiments succeeded:
  - GIMP (native aarch64) under muvm with sommelier.
  - Krita (x86_64 AppImage) under muvm+FEX using host-side SquashFS extraction and PYTHONHOME.

## Key local state

- muvm PR branch (local only): third_party/muvm branch guest-raise-max-map-count
  - commit: 8fe99a0 guest: raise vm.max_map_count early
- Krita artifacts:
  - AppImage: .local/apps/krita-5.2.13-x86_64.AppImage
  - Extracted tree: .local/apps/krita-5.2.13-host-extract/squashfs-root

## Krita working launch command

muvm --emu=fex -e PYTHONHOME=/home/wycats/Code/Personal/asahi/.local/apps/krita-5.2.13-host-extract/squashfs-root/usr \
 /home/wycats/Code/Personal/asahi/.local/apps/krita-5.2.13-host-extract/squashfs-root/AppRun

## RFC drafted

- docs/rfcs/stage-0/0003-generic-appimage-runner-for-muvm-fex.md

## Next concrete step

- Implement a generic AppImage runner in Rust:
  - detect SquashFS payload offset (validate superblock candidates)
  - extract via unsquashfs -o offset
  - run AppRun with minimal environment, plus user-provided env overrides

## Useful references

- Research notes:
  - docs/agent-context/research/muvm-gui-smoketest-gimp.md
  - docs/agent-context/research/muvm-fex-smoketest-krita-appimage.md
