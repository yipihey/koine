// The theme's proof (docs/plan-koine-theme.md TH-D5): every page and every
// tour surface of a running koine-host, screenshotted at an iPhone viewport
// (light and dark) and a Mac one. Driven by scripts/screenshots.sh, which
// builds the bundle, starts the host and sets the environment:
//
//   KOINE_URL   the host's origin (http://127.0.0.1:23188)
//   KOINE_OUT   where the PNGs go (docs/screenshots)
//
// Playwright is the global install (`npm root -g`), its Chromium under
// PLAYWRIGHT_BROWSERS_PATH; nothing is downloaded here.

import { createRequire } from "node:module";
import { execSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import path from "node:path";

const require = createRequire(import.meta.url);

function loadPlaywright() {
  try {
    return require("playwright");
  } catch {
    const root = execSync("npm root -g", { encoding: "utf8" }).trim();
    return require(path.join(root, "playwright"));
  }
}

const { chromium } = loadPlaywright();

const origin = (process.env.KOINE_URL || "http://127.0.0.1:23188").replace(/\/$/, "");
const out = process.env.KOINE_OUT || "docs/screenshots";
mkdirSync(out, { recursive: true });

// The viewports: an iPhone 13 at 3x, light and dark; a Mac window, light.
const viewports = [
  { name: "iphone", scheme: "light", viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true },
  { name: "iphone", scheme: "dark", viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true },
  { name: "mac", scheme: "light", viewport: { width: 1280, height: 800 }, deviceScaleFactor: 2, isMobile: false, hasTouch: false },
];

// The pages: the four of the shell, then every surface the host lists.
async function listSurfaces() {
  const answer = await fetch(`${origin}/api/surface`);
  if (!answer.ok) throw new Error(`GET /api/surface: ${answer.status}`);
  const body = await answer.json();
  const list = Array.isArray(body) ? body : body.surfaces || [];
  // The file is named by the title (the ids are minted at every launch).
  return list.map((s) => ({ id: s.id, name: slug(s.name || s.id) }));
}

function slug(text) {
  return text.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "surface";
}

const pages = [
  { name: "surfaces", path: "/ui/" },
  { name: "host", path: "/ui/host" },
  { name: "layout", path: "/ui/layout" },
  { name: "style", path: "/ui/style" },
];
for (const { id, name } of await listSurfaces()) {
  pages.push({ name: `surface-${name}`, path: `/ui/surface/${encodeURIComponent(id)}` });
}

const browser = await chromium.launch();
try {
  for (const vp of viewports) {
    const context = await browser.newContext({
      viewport: vp.viewport,
      deviceScaleFactor: vp.deviceScaleFactor,
      isMobile: vp.isMobile,
      hasTouch: vp.hasTouch,
      colorScheme: vp.scheme,
    });
    const page = await context.newPage();
    const failures = [];
    page.on("pageerror", (e) => failures.push(String(e)));
    for (const p of pages) {
      // Not `networkidle`: a surface page long-polls its wait route.
      await page.goto(`${origin}${p.path}`, { waitUntil: "load" });
      // The page lists and renders after its first fetch: wait for the
      // surface, the list or the host's pairs, then a beat for the paint.
      await page.waitForSelector(".koine, .kw-surfaces, .kw-host dl", { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(1000);
      const file = path.join(out, `${p.name}-${vp.name}-${vp.scheme}.png`);
      await page.screenshot({ path: file, fullPage: false });
      // A page wider than the viewport is the failure the theme exists to
      // stop (on a phone it also unpins the tab bar), so it is called out.
      const width = await page.evaluate(() => document.documentElement.scrollWidth);
      const overflow = width > vp.viewport.width ? `  OVERFLOW ${width}>${vp.viewport.width}` : "";
      console.log(`${file}${overflow}`);
    }
    if (failures.length) console.log(`page errors (${vp.name} ${vp.scheme}):\n  ${failures.join("\n  ")}`);
    await context.close();
  }
} finally {
  await browser.close();
}
