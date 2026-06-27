#!/usr/bin/env bash
# Build the Cerena browser bundle: compile arena-client to wasm and stamp a cache-busting
# version into index.html + boot.js (a matched glue+wasm pair per build — see boot.js).
#
# The output (this `web/` dir, with `pkg/` populated) is the content-addressed bundle
# ce-serve ships: each file is a blob, the bundle manifest is `{ spa: true, files }`, and
# ce-hub maps the Host -> bundle CID. ce-serve injects window.__ceNode into every page, so
# the wasm client reaches the mesh with no remote origin and each tab hosts its own zones.
#
# Usage:   web/build.sh            # build + stamp
#          web/build.sh --serve    # build, then `python3 -m http.server` for a local look
#                                   #   (note: local serving has NO mesh bridge; use ce-serve
#                                   #    for a real run — the boot shows "no mesh bridge").
set -euo pipefail

cd "$(dirname "$0")/.."   # cerena/ workspace root
WEB="web"

command -v wasm-pack >/dev/null 2>&1 || {
  echo "wasm-pack not found. Install: cargo install wasm-pack" >&2
  exit 1
}

echo "==> building arena-client -> wasm (web target)"
# --no-typescript keeps the bundle lean; --target web emits an ES module we import in boot.js.
wasm-pack build crates/arena-client --release --target web --no-typescript --out-dir "../../$WEB/pkg"

# Cache-bust token = short hash of the built wasm, so a new build is always a fresh URL.
WASM="$WEB/pkg/arena_client_bg.wasm"
[ -f "$WASM" ] || { echo "expected $WASM not found after build" >&2; exit 1; }
V="$(shasum -a 256 "$WASM" | cut -c1-12)"
echo "==> stamping bundle version $V"
# Replace the __CERENAV__ placeholder in a COPY so the source stays a clean template.
for f in index.html boot.js; do
  sed "s/__CERENAV__/$V/g" "$WEB/$f" > "$WEB/$f.stamped"
  mv "$WEB/$f.stamped" "$WEB/$f"
done

echo "==> bundle ready in $WEB/ (index.html, boot.js, pkg/)"
echo "    publish through ce-serve (content-addressed) and point a Host at it via ce-hub."

if [ "${1:-}" = "--serve" ]; then
  echo "==> local preview on http://localhost:8000 (NO mesh bridge — ce-serve required for hosting)"
  ( cd "$WEB" && python3 -m http.server 8000 )
fi
