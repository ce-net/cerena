// External boot module — kept out of the HTML so the page works under a strict
// `script-src 'self'` Content-Security-Policy (inline scripts are blocked, external
// same-origin ones are allowed).
//
// Transport: the client speaks the mesh through `window.__ceNode`, the bridge ce-serve
// injects into every page it serves (one same-origin WebSocket to /mesh-bridge, which the
// node authorises with its api.token, never exposed to the page). The wasm client
// (arena-client) then runs the authoritative replica engine for the zones around the
// player — this tab is a server, sharing authority with the other replicas by quorum.
//
// CACHE-BUST (critical): the glue JS that builds the wasm's import object and the .wasm are
// a MATCHED pair from one build — wasm-bindgen changes the import set between toolchain
// versions, and an edge can pair a fresh .wasm with stale glue, which fails instantiation
// and the page never boots. We load BOTH at a per-build version (`?v=<hash>`, stamped by
// the deploy step replacing __CERENAV__) so a new build is a new URL and always a matched pair.
const V = "__CERENAV__";

const boot = document.getElementById("bootmsg");
const setStatus = (m) => {
  const s = document.getElementById("hud-status");
  if (s) s.textContent = m;
};

(async () => {
  try {
    if (!globalThis.__ceNode) {
      // Not served through ce-serve: there is no mesh bridge, so hosting can't connect.
      if (boot) boot.textContent = "serve this page through ce-serve (no mesh bridge present)";
      setStatus("no mesh bridge — serve via ce-serve");
      return;
    }
    const mod = await import(`./pkg/arena_client.js?v=${V}`);
    await mod.default(`./pkg/arena_client_bg.wasm?v=${V}`);
    // `start()` (#[wasm_bindgen(start)]) has run; it brings up wgpu on #cerena-canvas and
    // connects the BrowserHost, which begins hosting the player's zones.
    const b = document.getElementById("boot");
    if (b) b.style.display = "none";
    setStatus("hosting your zones");
  } catch (e) {
    console.error("[boot] cerena failed to start:", e);
    if (boot) boot.textContent = "failed to boot: " + ((e && e.message) || e);
  }
})();
