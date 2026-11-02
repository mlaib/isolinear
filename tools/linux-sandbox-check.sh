#!/bin/bash
# Proves that opening documents makes no network connection and runs no program other than the
# app itself and the web view's own helper processes. Linux, with strace and an X display.
#
# Usage: DISPLAY=:99 tools/linux-sandbox-check.sh [FILE...]
# Without files it writes hostile samples (remote images, scripts, external entities) and checks
# those. Exits non-zero on any violation.
set -u
BIN=${ISOLINEAR_BIN:-target/release/isolinear}
# Every run is a first run: no cache (GStreamer's plugin registry among them) from an earlier one.
XDG_CACHE_HOME=$(mktemp -d)
export XDG_CACHE_HOME
WAIT=${ISOLINEAR_WAIT:-6}
[ -x "$BIN" ] || { echo "FAIL: $BIN is not built (npm run tauri build)"; exit 1; }
command -v strace >/dev/null || { echo "FAIL: strace is not installed"; exit 1; }

files=("$@")
if [ ${#files[@]} -eq 0 ]; then
  samples=$(mktemp -d)
  cat > "$samples/remote.md" <<'EOF'
# Remote content
![tracker](https://example.com/pixel.png) <img src="https://example.com/x.png">
<script>fetch("https://example.com/")</script> [link](https://example.com/)
EOF
  cat > "$samples/outputs.ipynb" <<'EOF'
{"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [{"cell_type": "code",
 "execution_count": 1, "metadata": {}, "source": "x", "outputs": [{"output_type": "display_data",
 "metadata": {}, "data": {"text/html": "<img src='https://example.com/a.png'><script>fetch('https://example.com')</script>",
 "application/javascript": "fetch('https://example.com')"}}]}]}
EOF
  cat > "$samples/external.svg" <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">
  <image xlink:href="https://example.com/a.png" width="10" height="10"/>
  <script>fetch("https://example.com/")</script>
</svg>
EOF
  cat > "$samples/entity.xml" <<'EOF'
<?xml version="1.0"?>
<!DOCTYPE r [<!ENTITY ext SYSTEM "https://example.com/entity.txt">]>
<r>&ext;</r>
EOF
  printf 'a,b\n"=WEBSERVICE(""https://example.com"")",2\n' > "$samples/formula.csv"
  files=("$samples"/*)
fi

status=0
for f in "${files[@]}"; do
  [ -f "$f" ] || { echo "FAIL $f: no such file"; status=1; continue; }
  trace=$(mktemp)
  ISOLINEAR_DATA_DIR=$(mktemp -d) LIBGL_ALWAYS_SOFTWARE=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 \
    strace -f -qq -e trace=socket,connect,execve,execveat -o "$trace" "$BIN" --wait "$f" >/dev/null 2>&1 &
  pid=$!
  sleep "$WAIT"
  pkill -TERM -P "$pid" 2>/dev/null
  kill "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  inet=$(grep -cE 'socket\((AF_INET|AF_INET6|AF_PACKET)' "$trace")
  conns=$(grep -cE 'connect\(.*sa_family=AF_INET6?' "$trace")
  # The app itself, and WebKit's web and network processes, are the only programs allowed.
  execs=$(grep -E 'execve(at)?\(' "$trace" | grep -v ' = -1 ' |
    grep -vE "execve\(\"[^\"]*/isolinear\"|webkit2gtk-4\.1/WebKit(Web|Network|GPU)Process\"" | wc -l)
  if [ "$inet" -eq 0 ] && [ "$conns" -eq 0 ] && [ "$execs" -eq 0 ]; then
    echo "PASS $(basename "$f"): no network sockets, no other program run"
  else
    echo "FAIL $(basename "$f"): inet_sockets=$inet inet_connects=$conns other_programs=$execs"
    grep -E 'AF_INET|execve' "$trace" | grep -v ' = -1 ' | head -8
    status=1
  fi
  rm -f "$trace"
done
exit $status
