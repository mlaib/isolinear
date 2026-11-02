#!/bin/sh
# Keeps Cargo's build directory in a cache folder between runs, so only changes rebuild.
# The cache is emptied when it grows past 60 GiB.
set -eu
CACHE="${CI_TARGET_CACHE:-$HOME/ci-cache/isolinear-target}"
mkdir -p "$CACHE"
if [ "$(du -sk "$CACHE" | cut -f1)" -gt $((60 * 1024 * 1024)) ]; then
  rm -rf "$CACHE"
  mkdir -p "$CACHE"
fi
rm -rf target
ln -s "$CACHE" target
echo "target -> $CACHE"
