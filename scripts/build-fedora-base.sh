#!/bin/bash
set -euo pipefail

# Configuration
IMAGE_NAME="fedora-base.erofs"
ROOTFS_DIR="fedora-rootfs"
FEDORA_RELEASE="41" # Or Rawhide
ARCH="x86_64"

# Clean up
rm -rf "$ROOTFS_DIR" "$IMAGE_NAME"
mkdir -p "$ROOTFS_DIR"

# Install packages into rootfs
echo "Installing Fedora packages..."
dnf install --installroot="$PWD/$ROOTFS_DIR" \
    --releasever="$FEDORA_RELEASE" \
    --forcearch="$ARCH" \
    --setopt=install_weak_deps=False \
    --nodocs \
    -y \
    bash coreutils glibc glibc-all-langpacks ncurses systemd systemd-libs zlib \
    mesa-dri-drivers mesa-filesystem mesa-libEGL mesa-libGL mesa-libgbm mesa-libglapi mesa-vulkan-drivers vulkan-loader \
    libX11 libXau libXcb libXcomposite libXcursor libXdamage libXext libXfixes libXi libXinerama libXrandr libXrender libXxf86vm \
    libwayland-client libwayland-cursor libwayland-egl libwayland-server libxkbcommon libxkbcommon-x11 \
    alsa-lib gstreamer1 gstreamer1-plugins-base gstreamer1-plugins-good gstreamer1-plugins-bad-free pipewire-libs pulseaudio-libs \
    gtk3 webkit2gtk3 libnotify libsecret libsoup openssl pango cairo gdk-pixbuf2 \
    fuse-libs libstdc++ libuuid libxml2 freetype fontconfig

# Cleanup DNF metadata to save space
dnf clean all --installroot="$PWD/$ROOTFS_DIR"
rm -rf "$ROOTFS_DIR/var/cache/dnf"

# Build EROFS image
echo "Building EROFS image..."
mkfs.erofs -zlz4hc "$IMAGE_NAME" "$ROOTFS_DIR"

echo "Done: $IMAGE_NAME"
