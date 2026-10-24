#!/usr/bin/env bash
# Runs a teleplot binary from an empty directory and checks that the page and
# every client asset are served from the binary itself.
set -euo pipefail

bin=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
port=${SMOKE_PORT:-3917}
dir=$(mktemp -d)
trap '{ kill "$pid"; wait "$pid"; } 2>/dev/null || true; rm -rf "$dir"' EXIT

name=$(basename "$bin")
cp "$bin" "$dir/$name"
cd "$dir"
"./$name" serve --addr "127.0.0.1:$port" >server.log 2>&1 &
pid=$!

for _ in $(seq 1 50); do
  curl -sf -o /dev/null "http://127.0.0.1:$port/" && break
  sleep 0.1
done

status=0
for path in / /pkg/teleplot.js /pkg/teleplot.wasm /pkg/teleplot.css; do
  code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port$path")
  echo "$code $path"
  [ "$code" = 200 ] || status=1
done

# Browsers that accept brotli get the precompressed bundle.
encoding=$(curl -s -o /dev/null -D - -H 'Accept-Encoding: br, gzip' "http://127.0.0.1:$port/pkg/teleplot.wasm" | tr -d '\r' | sed -n 's/^content-encoding: //Ip')
echo "${encoding:-identity} /pkg/teleplot.wasm (Accept-Encoding: br, gzip)"
[ "$encoding" = br ] || status=1

if grep -q "client bundle not found" server.log; then
  echo "server reported a missing client bundle"
  status=1
fi

[ $status = 0 ] && echo "Smoke test passed." || { cat server.log; echo "Smoke test FAILED."; }
exit $status
