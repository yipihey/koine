// The gesture suite (docs/plan-koine-affordances.md A-D5): every gesture the
// tour offers, fired with one pointer (GESTURE_POINTER=mouse|touch) against a
// fresh demo host, each case judged by what the host then answers, never by
// the DOM alone. Run through scripts/tests/gestures/run.sh, which starts the
// host and runs this once per pointer.
import { createRequire } from "node:module";
import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";

const require = createRequire(import.meta.url);
let playwright;
try {
  playwright = require("playwright");
} catch {
  const root = execSync("npm root -g", { encoding: "utf8" }).trim();
  playwright = require(path.join(root, "playwright"));
}

const origin = process.env.KOINE_URL;
const token = readFileSync(process.env.KOINE_TOKEN_FILE, "utf8").trim();
const touch = process.env.GESTURE_POINTER === "touch";
const label = touch ? "touch" : "mouse";

let fails = 0;
const pass = (what) => console.log(`PASS  [${label}] ${what}`);
const fail = (what, why) => {
  console.log(`FAIL  [${label}] ${what}${why ? `  (${why})` : ""}`);
  fails++;
};

// ---- the host's answers
async function get(p) {
  const r = await fetch(`${origin}${p}`);
  if (!r.ok) throw new Error(`GET ${p}: ${r.status}`);
  return r.json();
}
async function verb(name, args) {
  const r = await fetch(`${origin}/api/verb/${name}`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify(args),
  });
  return r.json();
}
async function surfaceId(name) {
  const list = await get("/api/surface");
  return list.surfaces.find((s) => s.name === name)?.id;
}
// Every note, collapsed collections included (the tree's render lists only
// what is open): `notes` answers { id, parent, title } with keys for ids.
async function treeRows(_sid) {
  const rows = await verb("surface-demo-service_notes", {});
  return (Array.isArray(rows) ? rows : []).map((n) => ({ id: n.id, label: n.title, parent: n.parent ?? null }));
}
const byLabel = (rows, l) => rows.find((r) => r.label === l);
async function until(probe, ms = 8000) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    last = await probe();
    if (last) return last;
    await new Promise((r) => setTimeout(r, 200));
  }
  return null;
}
async function layoutOrder() {
  const t = (await get("/api/layout/tree")).layout;
  const win = t.windows[0];
  const root = t.tiles[String(win.root)];
  const c = root.container ?? {};
  return ((c.linear ?? c.tabs ?? c.grid ?? {}).children ?? []).map(Number);
}

// ---- the pointer: one drag, whichever pointer this run is
// `edge: "top"` ends on the top edge of `to`, where the gap before it is
// drawn while a row is moving; otherwise it ends on the row itself.
async function drag(page, cdp, from, to, edge) {
  const a = await from.boundingBox();
  const b = await to.boundingBox();
  const sx = a.x + a.width / 2, sy = a.y + a.height / 2;
  const ex = b.x + b.width / 3, ey = edge === "top" ? b.y + 1 : b.y + b.height / 2;
  const steps = 10;
  if (touch) {
    const tp = (x, y) => [{ x, y, id: 1, radiusX: 4, radiusY: 4, force: 1 }];
    await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: tp(sx, sy) });
    for (let i = 1; i <= steps; i++) {
      await cdp.send("Input.dispatchTouchEvent", {
        type: "touchMove",
        touchPoints: tp(sx + ((ex - sx) * i) / steps, sy + ((ey - sy) * i) / steps),
      });
      await page.waitForTimeout(30);
    }
    await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  } else {
    await page.mouse.move(sx, sy);
    await page.mouse.down();
    for (let i = 1; i <= steps; i++) {
      await page.mouse.move(sx + ((ex - sx) * i) / steps, sy + ((ey - sy) * i) / steps);
      await page.waitForTimeout(30);
    }
    await page.mouse.up();
  }
}
const press = (loc) => (touch ? loc.tap() : loc.click());
// A row's "…": always shown to a finger, revealed by hovering the row for a
// mouse (the Mac convention), so the mouse run hovers the row first.
async function openMenu(r) {
  if (!touch) await r.hover();
  await press(r.locator(".koine-more"));
  const menu = r.locator(".koine-menu");
  await menu.waitFor({ timeout: 5000 });
  return menu;
}

const browser = await playwright.chromium.launch();
const context = await browser.newContext(
  touch
    ? { viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true }
    : { viewport: { width: 1280, height: 900 } },
);
const page = await context.newPage();
const cdp = touch ? await context.newCDPSession(page) : null;
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));
// GESTURE_DEBUG=1 prints every event the page posts, in order.
if (process.env.GESTURE_DEBUG) {
  page.on("request", (r) => {
    if (r.method() === "POST" && r.url().includes("/api/surface/")) console.log("  post", r.postData()?.slice(0, 140));
  });
}

async function step(what, body) {
  try {
    await body();
  } catch (e) {
    fail(what, String(e).split("\n")[0]);
  }
}

// ---- notes
const notes = await surfaceId("Notes");
await page.goto(`${origin}/ui/surface/${encodeURIComponent(notes)}`);
const tree = page.locator('[data-koine-id="outline"]');
const row = (l) => tree.locator("[data-koine-row]").filter({ hasText: l }).first();
await row("Ideas").waitFor({ timeout: 20000 });

await step("move a note into another collection by its grip", async () => {
  const before = await treeRows(notes);
  const reading = byLabel(before, "Reading");
  await drag(page, cdp, row("Ideas").locator(".koine-grip"), row("Reading"));
  const ok = await until(async () => byLabel(await treeRows(notes), "Ideas")?.parent === reading.id);
  ok ? pass("move a note into another collection by its grip") : fail("move a note into another collection by its grip", "parent unchanged");
});

await step("rename a note from its row menu", async () => {
  await page.waitForTimeout(400);
  const menu = await openMenu(row("Experiments"));
  const entries = await menu.locator("li").allTextContents();
  if (!entries.includes("Rename") || !entries.includes("Delete")) fail("the row menu lists Rename and Delete", JSON.stringify(entries));
  await press(menu.locator("li", { hasText: "Rename" }).first());
  const input = tree.locator("input.koine-rename");
  await input.waitFor({ timeout: 5000 });
  await input.fill("Trials");
  await input.press("Enter");
  const ok = await until(async () => {
    const rows = await treeRows(notes);
    return byLabel(rows, "Trials") && !byLabel(rows, "Experiments");
  });
  ok ? pass("rename a note from its row menu") : fail("rename a note from its row menu");
});

await step("delete a note from its row menu", async () => {
  const menu = await openMenu(row("Notes on koine"));
  await press(menu.locator("li", { hasText: "Delete" }).first());
  const ok = await until(async () => !byLabel(await treeRows(notes), "Notes on koine"));
  ok ? pass("delete a note from its row menu") : fail("delete a note from its row menu");
});

const name = page.locator("input#koine-new-title");
await step("add a collection and a note into it", async () => {
  await name.fill("Errands");
  await press(page.getByRole("button", { name: "Add collection" }));
  const coll = await until(async () => byLabel(await treeRows(notes), "Errands"));
  if (!coll || coll.parent) return fail("add a collection", "no root named Errands");
  pass("add a collection");
  // The handler's last steps clear the field and refresh the tree: wait for
  // both before typing the next name, as a person's next tap always does.
  await row("Errands").waitFor({ timeout: 8000 });
  await until(async () => (await name.inputValue()) === "");
  await press(row("Errands").locator(".koine-tree-label"));
  await name.fill("Groceries");
  await press(page.getByRole("button", { name: "Add note" }));
  const note = await until(async () => byLabel(await treeRows(notes), "Groceries"));
  note && note.parent === coll.id ? pass("add a note into the selected collection") : fail(
        "add a note into the selected collection",
        JSON.stringify({
          notes: (await treeRows(notes)).map((r) => [r.label, r.parent]),
          active: await tree.locator("[data-koine-active]").allTextContents(),
          name: await name.inputValue(),
          banner: await page.locator(".kw-banner").allTextContents(),
          state: (await verb("surface-service_surface-state-get", { id: notes }))?.state,
        }),
      );
});

await step("write a note's body", async () => {
  const id = byLabel(await treeRows(notes), "Papers to read").id;
  const saved = (await verb("surface-demo-service_note-get", { id }))?.note?.body ?? "";
  await press(row("Papers to read").locator(".koine-tree-label"));
  const body = page.locator('[data-koine-id="body"] textarea, [data-koine-id="body"] input, #koine-body').first();
  await body.waitFor({ timeout: 5000 });
  // The editor may still show the previously selected note: wait, as a
  // person does, until it shows this note's text before typing over it.
  const loaded = await until(async () => (await body.inputValue()) === saved, 8000);
  if (!loaded) return fail("write a note's body", `the editor never showed the note's text (shows ${JSON.stringify(await body.inputValue())})`);
  await body.fill(`Written by the ${label} gesture suite.`);
  await body.blur();
  await press(page.getByRole("button", { name: "Save" }));
  const ok = await until(async () => {
    const r = await verb("surface-demo-service_note-get", { id });
    return r?.note?.body === `Written by the ${label} gesture suite.`;
  });
  ok
    ? pass("write a note's body")
    : fail("write a note's body", `note-get answered ${JSON.stringify((await verb("surface-demo-service_note-get", { id }))?.note?.body)}`);
});

await step("a refused verb is shown, not swallowed", async () => {
  await name.fill("");
  await press(page.getByRole("button", { name: "Add collection" }));
  const banner = page.locator(".kw-banner");
  const shown = await until(async () => ((await banner.count()) ? (await banner.first().textContent()) : null), 5000);
  shown && /title/i.test(shown) ? pass(`a refused verb is shown, not swallowed (${shown.trim()})`) : fail("a refused verb is shown, not swallowed", `banner: ${shown}`);
});

// ---- style: a colour slot is a pick-list and takes a token without a refusal
await step("pick a colour token for a style slot", async () => {
  await page.goto(`${origin}/ui/style`);
  const select = page.locator("select#koine-button");
  await select.waitFor({ timeout: 10000 });
  const options = await select.locator("option").allTextContents();
  await select.selectOption({ label: "accent" });
  await page.waitForTimeout(300);
  const refused = await page.locator(".kw-banner").count();
  const shown = await select.evaluate((e) => e.options[e.selectedIndex]?.textContent);
  options.includes("accent") && refused === 0 && shown === "accent"
    ? pass("pick a colour token for a style slot")
    : fail("pick a colour token for a style slot", `shows ${shown}, banners ${refused}`);
});

// ---- layout: reorder a tile by its row in the outline, then stack two as tabs
await step("reorder a layout tile from the outline", async () => {
  await page.goto(`${origin}/ui/layout`);
  const outline = page.locator(".koine-tree").first();
  const rows = outline.locator("[data-koine-row]");
  await rows.nth(2).waitFor({ timeout: 20000 });
  const before = await layoutOrder();
  const labels = await rows.allTextContents();
  await drag(page, cdp, rows.nth(2).locator(".koine-grip"), rows.nth(0), "top");
  const want = [before[2], before[0], before[1]];
  // The host's order, then the outline's: the next step measures these rows,
  // and on the phone layout the outline re-renders after the host answers.
  const ok =
    (await until(async () => JSON.stringify(await layoutOrder()) === JSON.stringify(want))) &&
    (await until(async () => (await rows.first().textContent()) === labels[2]));
  ok ? pass(`reorder a layout tile from the outline (${before} -> ${want})`) : fail("reorder a layout tile from the outline", `${before} became ${await layoutOrder()}, wanted ${want}`);
});

await step("stack a layout tile onto another as tabs", async () => {
  const outline = page.locator(".koine-tree").first();
  const rows = outline.locator("[data-koine-row]");
  const before = await layoutOrder();
  await drag(page, cdp, rows.nth(2).locator(".koine-grip"), rows.nth(0));
  const ok = await until(async () => {
    const after = await layoutOrder();
    return after.length === before.length - 1 ? after : null;
  });
  ok ? pass(`stack a layout tile onto another as tabs (${before} -> ${ok})`) : fail("stack a layout tile onto another as tabs", `${before} became ${await layoutOrder()}`);
});

// ---- ask an agent (docs/plan-koine-research-os.md G-D5): the nav's tab for
// either pointer, `a` on a surface for the keyboard, and the desk's answer.
const desk = async () => (await verb("agent-service_requests", {})).rows ?? [];
const askFrom = async (prompt) => {
  const box = page.locator("#kw-ask-prompt");
  await box.waitFor({ timeout: 10000 });
  await box.fill(prompt);
  await press(page.locator("#kw-ask-question"));
  return until(async () => (await desk()).find((r) => r.prompt === prompt));
};

await step("ask an agent from the nav", async () => {
  await page.goto(`${origin}/ui/`);
  const tab = page.locator(".kw-nav a", { hasText: "ask" });
  await tab.waitFor({ timeout: 10000 });
  await press(tab);
  const filed = await askFrom(`what is on this page? (${label})`);
  filed && filed.status === "open" && filed.kind === "ask"
    ? pass("ask an agent from the nav")
    : fail("ask an agent from the nav", `desk: ${JSON.stringify(await desk())}`);
});

if (!touch) {
  await step("a on a surface asks about that surface", async () => {
    await page.goto(`${origin}/ui/surface/${encodeURIComponent(notes)}`);
    await row("Reading").waitFor({ timeout: 20000 });
    await page.locator(".kw-surface").focus();
    await page.keyboard.press("a");
    await page.waitForURL(/\/ui\/ask\?about=/, { timeout: 5000 });
    const filed = await askFrom("summarise these notes");
    const claimed = filed && (await verb("agent-service_claim", { id: filed.id }));
    const viewing = claimed?.request?.context?.viewing;
    viewing?.surface_id === notes && viewing?.name === "Notes"
      ? pass("a on a surface asks about that surface")
      : fail("a on a surface asks about that surface", JSON.stringify(claimed?.request?.context ?? claimed));
  });
}

await step("an answered view opens from the desk", async () => {
  const prompt = `show me the notes (${label})`;
  await page.goto(`${origin}/ui/ask`);
  const filed = await askFrom(prompt);
  await verb("agent-service_answer", { id: filed.id, text: "Here they are.", surface_id: notes });
  const item = page.locator(`[data-request="${filed.id}"]`);
  const link = item.locator("a", { hasText: "Open the view" });
  await link.waitFor({ timeout: 10000 });
  await press(link);
  await page.waitForURL(new RegExp(`/ui/surface/${notes}`), { timeout: 5000 });
  (await item.count()) >= 0 && page.url().includes(notes)
    ? pass("an answered view opens from the desk")
    : fail("an answered view opens from the desk", page.url());
});

if (errors.length) fail("no page errors", errors.join(" | "));
await browser.close();
console.log(`[${label}] ${fails ? `${fails} failed` : "all passed"}`);
process.exit(fails ? 1 : 0);
