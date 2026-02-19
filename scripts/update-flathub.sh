#!/bin/bash
set -e

# Source directory
SRC="src-tauri/flatpak"

# Destination directory (relative to ClusterCut repo root)
DEST="../flathub-clustercut"

if [ ! -d "$DEST" ]; then
    echo "Error: Destination directory $DEST does not exist."
    echo "Please clone the flathub repository to $DEST first."
    exit 1
fi

echo "Copying files from $SRC to $DEST..."

cp "$SRC/com.keithvassallo.clustercut.yml" "$DEST/"
cp "$SRC/cargo-sources.json" "$DEST/"
cp "$SRC/node-sources.json" "$DEST/"
cp "$SRC/com.keithvassallo.clustercut.metainfo.xml" "$DEST/"
cp "$SRC/com.keithvassallo.clustercut.svg" "$DEST/"
cp "$SRC/"*.patch "$DEST/"

echo "Files copied successfully to $DEST"
