# Bootstrap Runbook (Fedora Asahi Remix + GNOME)

This file is intentionally written as a **runbook**.

- Every change should be **empirical**: take a before/after snapshot and diff it.
- Every change should be **deletable**: include an explicit rollback.

## Evidence loop (use this for every change)

### Capture a baseline snapshot

If you have `asahi-setup` installed:

```bash
asahi-setup doctor --save
```

From the workspace:

```bash
cargo run -p asahi-setup -- doctor --save
```

If you want _only_ a saved artifact (no human output), use JSON and discard stdout:

```bash
asahi-setup doctor --save --json >/dev/null
```

### Make a single change

Apply exactly one change at a time so the diff stays explainable.

### Capture an after snapshot + diff

```bash
asahi-setup doctor --save
asahi-setup doctor-diff /path/to/older.json /path/to/newer.json
```

## 1) Critical system stability

### 1.1 Wi‑Fi stability: switch NetworkManager to iwd

**Goal**

- Replace `wpa_supplicant` with `iwd` (often more stable on Asahi).

**Apply**

```bash
sudo dnf install iwd
sudo mkdir -p /etc/NetworkManager/conf.d
sudo tee /etc/NetworkManager/conf.d/wifi_backend.conf >/dev/null <<'EOF'
[device]
wifi.backend=iwd
EOF

sudo systemctl stop wpa_supplicant
sudo systemctl disable wpa_supplicant

sudo systemctl enable --now iwd
sudo systemctl restart NetworkManager
```

**Verify**

```bash
systemctl is-active iwd
systemctl is-enabled iwd
systemctl is-active NetworkManager
```

Then do a before/after doctor snapshot and diff.

**Rollback**

```bash
sudo systemctl disable --now iwd
sudo systemctl enable --now wpa_supplicant
sudo rm -f /etc/NetworkManager/conf.d/wifi_backend.conf
sudo systemctl restart NetworkManager
```

### 1.2 Wi‑Fi tweaks (only if still unstable)

**Goal**

- Reduce firmware roaming/offload issues.

**Apply**

Disable firmware roaming & offload:

```bash
sudo tee /etc/modprobe.d/brcmfmac.conf >/dev/null <<'EOF'
options brcmfmac roamoff=1 feature_disable=0x82000
EOF
sudo reboot
```

Disable Wi‑Fi power management:

```bash
sudo mkdir -p /etc/NetworkManager/conf.d
sudo tee /etc/NetworkManager/conf.d/default-wifi-powersave-on.conf >/dev/null <<'EOF'
[connection]
wifi.powersave = 2
EOF
sudo systemctl restart NetworkManager
```

**Verify**

- If behavior improved, capture snapshots/diff.

**Rollback**

```bash
sudo rm -f /etc/modprobe.d/brcmfmac.conf
sudo rm -f /etc/NetworkManager/conf.d/default-wifi-powersave-on.conf
sudo reboot
```

### 1.3 Battery life tuning: auto-cpufreq

**Goal**

- Better CPU frequency management.

**Apply**

```bash
git clone https://github.com/AdnanHodzic/auto-cpufreq.git
cd auto-cpufreq
sudo ./auto-cpufreq-installer
```

**Verify**

- Confirm service/daemon (depends on installer version).

**Rollback**

- Use the upstream uninstall instructions.

### 1.4 Multimedia codecs: RPM Fusion

**Goal**

- Enable H.264/H.265 and other non-free codecs.

**Apply**

```bash
sudo dnf install \
  https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm \
  https://mirrors.rpmfusion.org/nonfree/fedora/rpmfusion-nonfree-release-$(rpm -E %fedora).noarch.rpm

sudo dnf groupupdate core
sudo dnf groupupdate multimedia --setopt="install_weak_deps=False" --exclude=PackageKit-gstreamer-plugin
sudo dnf groupupdate sound-and-video
```

**Verify**

- Play a known H.264 video.

**Rollback**

- Remove RPM Fusion repos + codec packages if needed.

## 2) Mac-like input + shortcuts (keyd + GNOME)

The project’s default approach is:

- Use `keyd` for kernel-level remapping.
- Keep terminal-friendly copy/paste via CUA (`Cmd+C` → `Ctrl+Insert`).
- Fix GNOME keybinding conflicts empirically.

### 2.1 Install keyd

**Goal**

- Install `keyd` and run it as a system service.

**Apply**

```bash
sudo dnf install make gcc git
git clone https://github.com/rvaiya/keyd
cd keyd
make
sudo make install
sudo systemctl enable --now keyd
```

**Verify**

```bash
systemctl is-active keyd
```

**Rollback**

```bash
sudo systemctl disable --now keyd
```

### 2.2 Configure the “Mac layer”

**Goal**

- Map Command to a layer that behaves like:
  - `Cmd+Tab` → window switcher (Alt+Tab behavior)
  - `Cmd+C/V/X` → CUA clipboard shortcuts
  - `Cmd+Space` → GNOME search

**Apply**

Write `/etc/keyd/default.conf`:

```ini
[ids]
*

[main]
# Map physical Left Command (Meta) to a custom layer.
leftmeta = layer(meta_mac)

# Map physical Left Alt (Option) to Meta (Super/Windows key) for OS shortcuts.
leftalt = layer(meta)

[meta_mac:A]
# Base layer: Alt.
# This keeps the GNOME window switcher UI open.

# Window switching
tab = tab
grave = grave

# Terminal/app compatibility: IBM CUA clipboard
c = C-insert
v = S-insert
x = S-delete

# Standard shortcuts: map back to Ctrl
a = C-a
b = C-b
d = C-d
e = C-e
f = C-f
g = C-g
h = C-h
i = C-i
j = C-j
k = C-k
l = C-l
m = C-m
n = C-n
o = C-o
p = C-p
q = C-q
r = C-r
s = C-s
t = C-t
u = C-u
w = C-w
y = C-y
z = C-z

# Common symbols
/ = C-/
. = C-.
, = C-,
[ = C-[]
] = C-]

# OS shortcuts
space = M-space
```

Reload keyd:

```bash
sudo keyd reload
```

**Verify**

- Confirm `Cmd+Tab`, `Cmd+Space`, and clipboard behavior.
- Capture snapshots + diff.

**Rollback**

- Restore the previous `/etc/keyd/default.conf` and reload keyd.

### 2.3 Prefer automation for “Spotlight fixes” (if using this repo)

This repo supports an idempotent “spotlight” target (keyd + GNOME bindings):

**Apply**

```bash
cargo run -p asahi-setup -- apply spotlight
```

**Verify**

```bash
cargo run -p asahi-setup -- check spotlight
cargo run -p asahi-setup -- doctor --save
```

**Rollback**

- Revert the keyd + gsettings deltas shown by `doctor-diff`.

## 3) Trackpad ergonomics

### 3.1 Tap-to-drag + drag lock

**Goal**

- Reduce dropped drags by enabling tap-to-drag and drag lock.

**Apply**

```bash
gsettings set org.gnome.desktop.peripherals.touchpad tap-and-drag true
gsettings set org.gnome.desktop.peripherals.touchpad tap-and-drag-lock true
```

**Verify**

- Try selecting text with tap-drag.
- Capture snapshots + diff.

**Rollback**

```bash
gsettings reset org.gnome.desktop.peripherals.touchpad tap-and-drag
gsettings reset org.gnome.desktop.peripherals.touchpad tap-and-drag-lock
```

### 3.2 Flat accel profile

**Goal**

- Force a flat acceleration profile.

**Apply**

```bash
gsettings set org.gnome.desktop.peripherals.touchpad accel-profile 'flat'
```

**Verify**

- Adjust “Touchpad Speed” in Settings and test.
- Capture snapshots + diff.

**Rollback**

```bash
gsettings reset org.gnome.desktop.peripherals.touchpad accel-profile
```

### 3.3 Palm rejection: titdb (“Trackpad Is Too Damn Big”)

**Goal**

- Create dead zones to ignore resting palms.

**Apply**

```bash
sudo dnf install git cmake libevdev-devel gcc-c++
git clone https://github.com/tascvh/trackpad-is-too-damn-big.git
cd trackpad-is-too-damn-big
mkdir build
cd build
cmake ..
make
sudo cp titdb /usr/local/bin/
```

Install and enable a `titdb.service`:

```bash
sudo tee /etc/systemd/system/titdb.service >/dev/null <<'EOF'
[Unit]
Description=Trackpad Is Too Damn Big Daemon
After=multi-user.target

[Service]
# Creates a dead zone frame around the trackpad to ignore resting palms.
# Note: on this titdb build, the device argument (-d) is mandatory.
# -b 25: Disable bottom 25% (main resting area)
# -l 20: Disable left 20%
# -r 20: Disable right 20%
# -t 5: Disable top 5% (near keyboard)
# IMPORTANT: use a stable /dev/input/by-path/... symlink, not /dev/input/eventX.
# event numbers can change across boots, which makes the service flaky.
ExecStart=/usr/local/bin/titdb -b 25 -l 20 -r 20 -t 5 -d /dev/input/by-path/platform-…-event-mouse
Restart=on-failure

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now titdb.service
```

Enable GNOME-native “disable while typing” for defense-in-depth:

```bash
gsettings set org.gnome.desktop.peripherals.touchpad disable-while-typing true
```

**Verify**

```bash
systemctl is-active titdb
journalctl -u titdb.service -b --no-pager | tail -n 80
```

If you have this repo checked out, the safe/reliable path is to let `asahi-setup` pick the stable device path and update the unit for you:

```bash
asahi-setup check titdb
asahi-setup apply titdb
```

If you’re editing manually, determine the touchpad’s event node and map it to a stable `/dev/input/by-path/*event-mouse` symlink:

```bash
sudo libinput list-devices
ls -la /dev/input/by-path | grep event-mouse
```

If systemd says “Start request repeated too quickly”, clear the start-limit and try again:

```bash
sudo systemctl reset-failed titdb.service
sudo systemctl start titdb.service
```

Then use `asahi-setup doctor` to verify probes and capture snapshots/diff.

**Rollback**

```bash
sudo systemctl disable --now titdb.service
sudo rm -f /etc/systemd/system/titdb.service
sudo systemctl daemon-reload

sudo rm -f /usr/local/bin/titdb
```

## 4) Display + notch

### 4.1 “Golden ratio” scaling

**Goal**

- Avoid fractional scaling overhead while getting usable text size.

**Apply**

- Set Display Scale to 200% in Settings → Displays.
- In GNOME Tweaks/Refine, set Fonts scaling factor to ~0.80–0.85.

### 4.2 Notch management

**Apply**

- Install “Just Perfection”: https://extensions.gnome.org/extension/3843/just-perfection/

### 4.3 Ultrawide ergonomics (tiling)

**Apply**

```bash
flatpak install flathub com.mattjakeman.ExtensionManager
gsettings set org.gnome.mutter edge-tiling false
```

Then install/configure “Tiling Shell” via Extension Manager.

Note: this runbook assumes “Tiling Shell” is the preferred approach (it effectively supersedes simpler tiling extensions). Avoid stacking multiple tiling extensions unless you know exactly how they interact.

## 5) Visual polish

### 5.1 Fonts (Inter + JetBrains Mono)

```bash
sudo dnf install rsms-inter-fonts
mkdir -p ~/.local/share/fonts
cd ~/.local/share/fonts
wget https://github.com/ryanoasis/nerd-fonts/releases/latest/download/JetBrainsMono.zip
unzip -o JetBrainsMono.zip
rm JetBrainsMono.zip
fc-cache -fv
```

### 5.2 Battery percentage

```bash
gsettings set org.gnome.desktop.interface show-battery-percentage true
```

### 5.3 Refine

```bash
flatpak install flathub page.tesk.Refine
```

### 5.4 Dock

- Dash to Dock: https://extensions.gnome.org/extension/307/dash-to-dock/
- Dash to Panel: https://extensions.gnome.org/extension/1160/dash-to-panel/

### 5.5 Blur + icons

```bash
sudo dnf install papirus-icon-theme
```

Blur My Shell: https://extensions.gnome.org/extension/3193/blur-my-shell/

### 5.6 Cursor theme (Bibata Modern Ice)

**Goal**

- Install a modern cursor theme.

**Apply**

```bash
mkdir -p /tmp/bibata
cd /tmp/bibata
wget https://github.com/ful1e5/Bibata_Cursor/releases/latest/download/Bibata-Modern-Ice.tar.gz
tar -xvf Bibata-Modern-Ice.tar.gz
mkdir -p ~/.local/share/icons
mv Bibata-Modern-Ice ~/.local/share/icons/
```

**Verify**

- GNOME Tweaks → Appearance → Cursor.

**Rollback**

```bash
rm -rf ~/.local/share/icons/Bibata-Modern-Ice
```

### 5.7 Legacy apps: adw-gtk3

**Goal**

- Make GTK3/legacy apps visually match modern Libadwaita styling.

**Apply**

```bash
sudo dnf install adw-gtk3-theme
```

**Verify**

- GNOME Tweaks → Appearance → Legacy Applications.

**Rollback**

```bash
sudo dnf remove adw-gtk3-theme
```

### 5.8 Optional: font discovery GUI (Font Downloader + Flatseal)

**Goal**

- Browse/install fonts via GUI.

**Apply**

```bash
flatpak install flathub org.gustavoperrot.FontDownloader
flatpak install flathub com.github.tchx84.Flatseal
```

**Verify**

- In Flatseal, grant Font Downloader access to `~/.local/share/fonts`.

## 6) Power-user tools

### 6.1 Faster DNF

- Edit `/etc/dnf/dnf.conf` and add:

```ini
max_parallel_downloads=10
fastestmirror=True
defaultyes=True
```

### 6.2 Allow GNOME Software on metered

```bash
gsettings set org.gnome.software download-updates-on-metered true
```

### 6.3 Terminals

```bash
flatpak install flathub com.raggesilver.BlackBox
sudo dnf install alacritty
```

ddterm: https://extensions.gnome.org/extension/3780/ddterm/

### 6.4 Keyring + settings inspection

**Goal**

- Fix “OS keyring is not available” errors and enable raw settings inspection.

**Apply**

```bash
sudo dnf install libsecret seahorse dconf-editor
```

**Verify**

- Open “Passwords and Keys” (Seahorse) and confirm the “Login” keyring is unlocked.

**Rollback**

```bash
sudo dnf remove dconf-editor seahorse libsecret
```

### 6.5 Browsers with sync (ARM64 reality check)

**Goal**

- Get a browser with reliable sync on ARM64.

**Apply**

- Microsoft Edge: download the Linux `.rpm` from https://www.microsoft.com/edge and install it.

**Verify**

- Sign in and confirm sync is active.

**Notes**

- Chrome availability on Linux ARM64 has historically been uneven; if you need sync and native speed, Edge is often the path of least resistance.
- Chromium is available via Fedora packages (`sudo dnf install chromium`) but may not provide the same sync experience.

### 6.6 Automated “Storage Sense” for Downloads (systemd-tmpfiles)

**Goal**

- Automatically delete old files from `~/Downloads`.

**Apply**

```bash
mkdir -p ~/.config/user-tmpfiles.d
tee ~/.config/user-tmpfiles.d/downloads.conf >/dev/null <<'EOF'
# Delete files older than 30 days from ~/Downloads
d %h/Downloads - - - 30d -
EOF

systemctl --user enable --now systemd-tmpfiles-clean.timer
```

**Verify**

```bash
systemctl --user status systemd-tmpfiles-clean.timer
```

**Rollback**

```bash
rm -f ~/.config/user-tmpfiles.d/downloads.conf
```

## 7) Compatibility + hardware

### 7.1 muvm

```bash
sudo dnf install muvm fex-emu
```

### 7.2 Thunderbolt docks

```bash
sudo dnf install bolt
boltctl list
```

If you need to rescan PCI:

```bash
echo 1 | sudo tee /sys/bus/pci/rescan
```

## 8) Audio

```bash
flatpak install flathub com.github.wwmm.easyeffects
```

Safety note: avoid boosting input gain above 0dB.
