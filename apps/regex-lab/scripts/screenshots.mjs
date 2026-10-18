// regex-lab's walk-through, screenshotted: the tester, a refused pattern,
// the explanation, the registry package installed, its recipes saved into
// the library, a saved pattern tried, log lines as a table, and the form
// koine generates for one verb with no page written for it.
//
//   REGEX_LAB_URL  the running host (http://127.0.0.1:23131)
//   REGEX_LAB_OUT  where the PNGs go (docs/screenshots)
//   REGEX_LAB_TOKEN_FILE  the host's loopback token file (for the install)
//
// The host must have been started with --registry <this app>/registry.

import { createRequire } from "node:module";
import { execSync } from "node:child_process";
import { mkdirSync, readFileSync } from "node:fs";
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

const origin = (process.env.REGEX_LAB_URL || "http://127.0.0.1:23131").replace(/\/$/, "");
const out = process.env.REGEX_LAB_OUT || "docs/screenshots";
const token = readFileSync(process.env.REGEX_LAB_TOKEN_FILE, "utf8").trim();
mkdirSync(out, { recursive: true });

async function verb(name, args) {
  const answer = await fetch(`${origin}/api/verb/${name}`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify(args),
  });
  const body = await answer.json();
  if (body.ok === false) throw new Error(`${name}: ${JSON.stringify(body)}`);
  return body;
}

async function surfaceId(name) {
  const body = await (await fetch(`${origin}/api/surface`)).json();
  const list = Array.isArray(body) ? body : body.surfaces;
  const found = list.find((s) => s.name === name);
  if (!found) throw new Error(`no surface named ${name}: ${list.map((s) => s.name)}`);
  return found.id;
}

const browser = await chromium.launch();
const context = await browser.newContext({
  viewport: { width: 1280, height: 800 },
  deviceScaleFactor: 2,
  colorScheme: "light",
});
const page = await context.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));

async function open(pathname) {
  await page.goto(`${origin}/ui${pathname}`, { waitUntil: "load" });
  await page.waitForTimeout(1500);
}
async function shot(name) {
  await page.waitForTimeout(800);
  // A full-page capture repeats a sticky bar where the viewport ended.
  await page.evaluate(() => {
    for (const el of document.querySelectorAll("*")) {
      const position = getComputedStyle(el).position;
      if (position === "sticky" || position === "fixed") el.style.position = "static";
    }
  });
  await page.screenshot({ path: path.join(out, `regex-lab-${name}.png`), fullPage: true });
  console.log(`regex-lab-${name}.png`);
}
// A field commits on change: type, then leave it.
async function type(id, text) {
  const field = page.locator(`[data-koine-id="${id}"]`);
  await field.fill(text);
  await field.press("Tab");
  await page.waitForTimeout(1200);
}
async function tab(title) {
  await page.getByRole("tab", { name: title }).click();
  await page.waitForTimeout(800);
}

// 1. The tester, as shipped.
const tester = await surfaceId("Regex tester");
await open(`/surface/${tester}`);
await shot("1-tester");

// 2. A pattern that does not parse: refused, with the place.
await type("pattern", "(?P<key>[a-z]+=(?P<value>\\S+)");
await shot("2-refused");

// 3. A date pattern, explained.
await type("pattern", "(?P<year>\\d{4})-(?P<month>\\d{2})-(?P<day>\\d{2})");
await type("text", "Submitted 2026-10-17, revised 2026-11-02.");
await tab("Explain");
await shot("3-explain");
await tab("Replace");
await type("replacement", "${day}/${month}/${year}");
await shot("4-replace");

// 5. The registry package: reviewed, installed; its pages appear.
const review = await verb("package-service_fetch", { name: "regex-recipes" });
await verb("package-service_install", { path: review.path, digest: review.digest });
await open("/");
await shot("5-surfaces");

// 6. The recipes saved into the library with one button.
await open(`/surface/${await surfaceId("Regex recipes")}`);
await page.getByRole("button", { name: "Save all to the library" }).click();
await page.waitForTimeout(2000);
await shot("6-recipes");

// 7. The library: pick arxiv-id, try it.
await open(`/surface/${await surfaceId("Pattern library")}`);
await page.locator('[data-koine-id="patterns-table"]').getByText("arxiv-id", { exact: true }).click();
await page.waitForTimeout(1200);
await type("try-text", "See 2310.06825v2 and 1706.03762 (v7 is the latest).");
await shot("7-library");

// 8. Log lines as a table.
await open(`/surface/${await surfaceId("Log fields")}`);
await shot("8-log-fields");

// 9. The form koine generates for a verb, with nothing written for it.
const form = await (await fetch(`${origin}/api/verb/regex-lab-service_split/surface`)).json();
const created = await (
  await fetch(`${origin}/api/surface`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify(form.spec),
  })
).json();
await open(`/surface/${created.id || created.surface?.id}`);
await page.getByRole("button", { name: "Run" }).click();
await page.waitForTimeout(1500);
await shot("9-generated-form");

// 10. The tester on a phone, dark.
const phone = await browser.newContext({
  viewport: { width: 390, height: 844 },
  deviceScaleFactor: 3,
  isMobile: true,
  hasTouch: true,
  colorScheme: "dark",
});
const phonePage = await phone.newPage();
await phonePage.goto(`${origin}/ui/surface/${tester}`, { waitUntil: "load" });
await phonePage.waitForTimeout(2000);
await phonePage.screenshot({ path: path.join(out, "regex-lab-10-iphone-dark.png") });
console.log("regex-lab-10-iphone-dark.png");

await browser.close();
if (errors.length) {
  console.error(errors.join("\n"));
  process.exit(1);
}
