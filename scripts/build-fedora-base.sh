#!/bin/bash
set -euo pipefail

# Ensure we are at the project root
if [ ! -f "Cargo.toml" ]; then
    echo "Error: Please run this script from the project root."
    exit 1
fi

# Build dependencies
echo "Building fedora-builder..."
cargo build --bin fedora-builder

echo "Building muvm..."
cargo build --manifest-path third_party/muvm/Cargo.toml --bin muvm --bin muvm-guest

# Define paths
MUVM="./third_party/muvm/target/debug/muvm"
OUTPUT="fedora-base.erofs"

# Check if muvm exists
if [ ! -f "$MUVM" ]; then
    echo "Error: muvm binary not found at $MUVM"
    exit 1
fi

# Run in VM
echo "Running build in muvm..."
# We use a complex bash command to setup a tmpfs workspace, run the builder, and copy the result back.
# This avoids permission issues with dnf on the host mount and ensures a clean build environment.
$MUVM --privileged -- bash -c "
    set -e
    
    # Navigate to project root in guest (assuming standard muvm mount)
    # We use the host's PWD to find the path
    HOST_PWD=\"$(pwd)\"
    if [ -d \"\$HOST_PWD\" ]; then
        cd \"\$HOST_PWD\"
    else
        echo \"Error: Could not find project directory \$HOST_PWD in guest\"
        exit 1
    fi

    echo \"Setting up tmpfs workspace...\"
    mkdir -p /tmp/build
    mount -t tmpfs tmpfs /tmp/build
    
    echo \"Copying builder to workspace...\"
    cp target/debug/fedora-builder /tmp/build/
    
    echo \"Running fedora-builder...\"
    cd /tmp/build
    ./fedora-builder --release 41 --arch aarch64 --output fedora-base.erofs
    
    echo \"Copying artifact back to host...\"
    cp fedora-base.erofs \"\$HOST_PWD/$OUTPUT\"
    
    echo \"Build complete inside VM.\"
"

echo "Success! Artifact available at $OUTPUT"
ls -lh "$OUTPUT"
