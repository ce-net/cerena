#!/usr/bin/env node
// Cerena visual smoke test — automated, headless, repeatable.
//
// Drives a real browser against a Cerena deployment (the live site by default),
// waits for the wasm client to boot + the player to spawn, then proves the frontend
// and the backend simulation are actually agreeing:
//
//   * the mesh bridge connects and the tab starts hosting its zones,
//   * the player spawns (the HUD reports a position and health from the sim),
//   * the 3D view is NOT a near-empty sky (terrain is actually drawn and you are not
//     stuck under the ground — the classic "spawned inside the terrain, see through
//     the back-culled hull, all sky" failure),
//   * no uncaught page errors.
//
// It prints a report, writes a screenshot, and exits non-zero if any check fails — so
// it works by hand AND in CI / from another agent. This is the tool to reach for when
// verifying any change to the renderer, the spawn/worldgen, or the HUD wiring; extend
// the CHECKS below as the client grows.
//
// Usage:
//   npm install                         # once, pulls puppeteer-core
//   node smoke.mjs                       # test https://cerena.ce-net.com
//   node smoke.mjs http://localhost:8790 # test another origin
//   URL=... WAIT_MS=60000 OUT=shot.png SHOW=1 node smoke.mjs
//
// Env:
//   URL      target origin (default https://cerena.ce-net.com/)
//   WAIT_MS  max time to wait for spawn before giving up (default 70000)
//   OUT      screenshot path (default ./cerena-smoke.png)
//   SHOW=1   run headful (a visible window) instead of headless
//   CHROME   path to the chrome/chromium binary (else auto-detected)
//
// GPU note: the renderer needs a WebGL2 context. If an X display + DRI device are
// present we drive the real GPU (ANGLE-over-GL); otherwise we fall back to SwiftShader.
// SwiftShader mis-reports the Depth32Float attachment ("framebufferTexture2D: invalid
// attachment") and refuses to draw geometry, so prefer a real GPU for a meaningful
// terrain/entity check; the spawn + HUD checks work under either.

import { existsSync, writeFileSync } from 'node:fs';
import puppeteer from 'puppeteer-core';
import { PNG } from 'pngjs';

const URL = process.argv[2] || process.env.URL || 'https://cerena.ce-net.com/';
const WAIT_MS = parseInt(process.env.WAIT_MS || '70000', 10);
const OUT = process.env.OUT || new URL('./cerena-smoke.png', import.meta.url).pathname;

// The opaque-pass clear colour (render.rs), sRGB-encoded — what an empty / underground
// view is filled with. We treat pixels near this as "sky", the rest as world geometry.
const SKY = [48, 56, 75];
const SKY_TOL = 10;
// A healthy first-person view on the surface is well under this much sky.
const MAX_SKY_FRACTION = 0.92;

function findChrome() {
  if (process.env.CHROME) return process.env.CHROME;
  const candidates = [
    '/usr/bin/google-chrome-stable',
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  ];
  return candidates.find((p) => existsSync(p)) || candidates[0];
}

function gpuArgs() {
  const hasDisplay = !!process.env.DISPLAY;
  const hasDri = existsSync('/dev/dri');
  if (hasDisplay && hasDri) {
    // Real GPU through ANGLE — a spec-compliant WebGL2 context — plus a Vulkan-backed
    // WebGPU adapter (the renderer prefers WebGPU and only falls back to WebGL2). The
    // unsafe-webgpu/Vulkan flags expose WebGPU in headless Chrome so this tool can test
    // the WebGPU path; without them Chrome offers no WebGPU adapter on a server box.
    return {
      headful: true,
      args: [
        '--use-gl=angle',
        '--use-angle=gl',
        '--ignore-gpu-blocklist',
        '--enable-unsafe-webgpu',
        '--enable-features=Vulkan,WebGPU',
      ],
    };
  }
  // Software fallback.
  return {
    headful: false,
    args: ['--use-gl=angle', '--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'],
  };
}

const fails = [];
const fail = (m) => { fails.push(m); };

const gpu = gpuArgs();
const headful = process.env.SHOW === '1' ? true : gpu.headful;
console.log(`cerena visual smoke → ${URL}`);
console.log(`  chrome=${findChrome()}  headful=${headful}  gpu=${gpu.headful ? 'real' : 'swiftshader'}`);

const browser = await puppeteer.launch({
  executablePath: findChrome(),
  headless: headful ? false : 'new',
  args: ['--no-sandbox', '--disable-setuid-sandbox', '--window-size=1280,800', ...gpu.args],
});

try {
  const page = await browser.newPage();
  await page.setViewport({ width: 1280, height: 720 });

  const logCounts = new Map();
  const errors = [];
  page.on('console', (m) => {
    const t = `[${m.type()}] ${m.text()}`.replace(/%c/g, '').replace(/color:[^;]*;?/g, '').replace(/\s+/g, ' ').trim();
    logCounts.set(t, (logCounts.get(t) || 0) + 1);
  });
  page.on('pageerror', (e) => errors.push(e.message));

  await page.goto(URL, { waitUntil: 'load', timeout: 30000 });

  // Poll the HUD until the sim reports a spawned player (a coordinate appears), or the
  // budget runs out. This is faster and more reliable than a fixed sleep.
  const t0 = Date.now();
  let spawned = false;
  let last = {};
  while (Date.now() - t0 < WAIT_MS) {
    last = await page.evaluate(() => ({
      hasBridge: !!globalThis.__ceNode,
      boot: document.getElementById('bootmsg')?.textContent ?? '',
      status: document.getElementById('hud-status')?.textContent ?? '',
      hp: document.getElementById('hp-label')?.textContent ?? '',
      coord: document.getElementById('hud-coord')?.textContent ?? '',
      peers: document.getElementById('hud-peers')?.textContent ?? '',
    }));
    if (last.coord && /\d/.test(last.coord) && last.hp && last.hp !== '—') { spawned = true; break; }
    await new Promise((r) => setTimeout(r, 1000));
  }
  const spawnSecs = ((Date.now() - t0) / 1000).toFixed(1);

  // Analyse the rendered frame from the SCREENSHOT, not an in-page canvas readback:
  // the WebGL context is created without `preserveDrawingBuffer`, so reading the
  // canvas back in JS returns a blank buffer at an arbitrary moment (a false "nothing
  // rendered"). The compositor screenshot always reflects the real presented frame.
  const analyse = (buf) => {
    const { width: w, height: h, data: d } = PNG.sync.read(buf);
    let sky = 0, total = 0; const uniq = new Set();
    for (let y = 0; y < h; y += 3) {
      for (let x = 0; x < w; x += 3) {
        const i = (y * w + x) * 4;
        total++;
        uniq.add(`${d[i]},${d[i + 1]},${d[i + 2]}`);
        if (Math.abs(d[i] - SKY[0]) <= SKY_TOL && Math.abs(d[i + 1] - SKY[1]) <= SKY_TOL && Math.abs(d[i + 2] - SKY[2]) <= SKY_TOL) sky++;
      }
    }
    return { skyFraction: sky / total, distinctColors: uniq.size, w, h };
  };
  // Terrain streams in one zone per frame after spawn, so give it a few seconds to
  // settle and measure the steady state (re-screenshot until the view stops being
  // mostly sky, or a short budget elapses). This keeps the verdict about the actual
  // playable view, not a half-streamed first frame.
  let shot = Buffer.from(await page.screenshot({ type: 'png' }));
  let pix = analyse(shot);
  for (let i = 0; i < 8 && pix.skyFraction > MAX_SKY_FRACTION; i++) {
    await new Promise((r) => setTimeout(r, 1500));
    shot = Buffer.from(await page.screenshot({ type: 'png' }));
    pix = analyse(shot);
  }
  writeFileSync(OUT, shot);

  // Measure sustained frame rate (rAF count over a 2s window) — turns "feels laggy"
  // into a number. Done after terrain has settled so it reflects the steady state.
  const fps = await page.evaluate(() => new Promise((res) => {
    let n = 0; const t0 = performance.now();
    const tick = () => {
      n++;
      const el = performance.now() - t0;
      if (el < 2000) requestAnimationFrame(tick);
      else res(Math.round((n / el) * 1000));
    };
    requestAnimationFrame(tick);
  }));

  // ---- checks ----
  if (fps < 24) fail(`only ${fps} fps — render/sim is too slow to play`);
  if (!last.hasBridge) fail('no ce-serve mesh bridge (window.__ceNode) — not served through ce-serve?');
  const connected = [...logCounts.keys()].some((l) => l.includes('browser host connected'));
  if (!connected) fail('client never logged "browser host connected"');
  if (!spawned) fail(`player did not spawn within ${(WAIT_MS / 1000) | 0}s (status="${last.status}")`);
  if (errors.length) fail(`page errors: ${errors.join(' | ')}`);
  if (gpu.headful && pix.skyFraction != null && pix.skyFraction > MAX_SKY_FRACTION) {
    fail(`view is ${(pix.skyFraction * 100).toFixed(0)}% sky (>${MAX_SKY_FRACTION * 100}%) — terrain not drawn / spawned under the ground`);
  }
  if (gpu.headful && pix.distinctColors != null && pix.distinctColors < 4) {
    fail(`canvas has only ${pix.distinctColors} distinct colours — nothing is rendering`);
  }

  // ---- report ----
  console.log('\n--- result ---');
  console.log(`bridge=${last.hasBridge} connected=${connected} spawned=${spawned} (${spawnSecs}s)`);
  console.log(`status="${last.status}" hp=${last.hp} coord="${last.coord}" peers="${last.peers}" fps=${fps}`);
  if (pix.skyFraction != null) console.log(`render: sky=${(pix.skyFraction * 100).toFixed(1)}% distinctColors=${pix.distinctColors} (${pix.w}x${pix.h})`);
  console.log(`screenshot → ${OUT}`);
  console.log('\nnotable console lines:');
  for (const [l, n] of logCounts) {
    if (/error|warn|panic|host connected|spawn|terrain|websocket/i.test(l)) console.log(`  x${n} ${l}`);
  }

  if (fails.length) {
    console.log(`\nFAIL (${fails.length}):`);
    for (const f of fails) console.log(`  ✗ ${f}`);
    process.exitCode = 1;
  } else {
    console.log('\nPASS — frontend and backend agree (booted, spawned, world rendered).');
  }
} finally {
  await browser.close();
}
