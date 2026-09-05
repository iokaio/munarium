// The operator console through a real browser.
//
// Dev-only, never in the image, never in the core build: `test.ps1 -BlackBox
// -Browser` runs it against compose, and its only outputs are (1) a pass/fail
// on the things a headless HTTP test cannot see — a page that RENDERS, a form
// that submits from a browser with a real Origin, a cookie the browser keeps
// — and (2) the screenshots under docs/guides/images/admin-ui/, which the
// guide embeds. The guide's screenshots come from HERE, not from a hand, so
// a page that changed and a picture that did not cannot coexist for long.
//
// Everything asserted is also asserted by the conformance admin tier over
// HTTP; this adds the browser, not the claims.
//
//   MUNARIUM_MATRIX_TEST_URL         http://127.0.0.1:8180 (default)
//   MUNARIUM_MATRIX_TEST_MGMT_TOKEN  mxmgmt (default; mxtest-mgmt on the estate)
//   MUNARIUM_MATRIX_SHOTS            where screenshots go (default: ../docs/guides/images/admin-ui)

import { chromium } from "playwright";
import { mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const base = (process.env.MUNARIUM_MATRIX_TEST_URL ?? "http://127.0.0.1:8180").replace(/\/$/, "");
const token = process.env.MUNARIUM_MATRIX_TEST_MGMT_TOKEN ?? "mxmgmt";
const shots = process.env.MUNARIUM_MATRIX_SHOTS ?? join(here, "..", "docs", "guides", "images", "admin-ui");
mkdirSync(shots, { recursive: true });

// The read pages, in the order the guide walks them. Each is screenshotted
// after it renders; each must contain no <script> — the console's CSP names
// no script source, and a page that shipped one would be blocked by the
// browser rather than merely flagged here.
const pages = [
  ["overview", "/admin"],
  ["sources", "/admin/sources"],
  ["source-crm", "/admin/sources/crm"],
  ["runs", "/admin/runs"],
  ["journal", "/admin/journal"],
  ["verification", "/admin/verification"],
  ["mappings", "/admin/mappings"],
  ["registry", "/admin/registry"],
  ["author", "/admin/author"],
];

const failures = [];
const check = (name, ok, detail = "") => {
  if (ok) console.log(`ok   ${name}`);
  else { console.log(`FAIL ${name} ${detail}`); failures.push(name); }
};

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, colorScheme: "light" });
const page = await ctx.newPage();

// 1. Unauthenticated: the console redirects to its login page rather than
//    rendering anything. A browser follows the redirect, which is the point
//    of doing this one from a browser.
await page.goto(`${base}/admin`);
check("anonymous /admin lands on the login page", page.url().endsWith("/admin/login"), page.url());
await page.screenshot({ path: join(shots, "login.png"), fullPage: true });

// 2. Log in through the FORM — the cookie flow, with the browser supplying
//    the Origin the console checks on every write.
await page.fill('input[name="token"]', token);
await Promise.all([page.waitForNavigation(), page.click('button[type="submit"], input[type="submit"]')]);
// The cookie is scoped to Path=/admin, so ask for it at that path — asking at
// the origin's root sees nothing, which is not the same as no cookie.
check("login form sets the session cookie", (await ctx.cookies(`${base}/admin`)).some((c) => c.httpOnly), JSON.stringify(await ctx.cookies(`${base}/admin`)));

// 3. Every read page renders, with no script, and gets its picture taken.
for (const [name, path] of pages) {
  const resp = await page.goto(`${base}${path}`);
  const html = await page.content();
  check(`${path} renders`, resp?.ok(), `status ${resp?.status()}`);
  check(`${path} carries no <script>`, !/<script\b/i.test(html));
  await page.screenshot({ path: join(shots, `${name}.png`), fullPage: true });
}

// 4. A write from the browser: probe `crm` through the source page's form.
//    The outcome does not matter (an unreachable source still journals); the
//    submission carrying a real Origin and the CSRF field does.
await page.goto(`${base}/admin/sources/crm`);
const rw = process.env.MUNARIUM_MATRIX_TEST_TOKEN ?? "mxdev";
const probeForm = page.locator('form[action="/admin/sources/crm/probe"]');
check("the source page offers the probe form", (await probeForm.count()) === 1);
if ((await probeForm.count()) === 1) {
  await probeForm.locator('input[name="rw_token"]').fill(rw);
  await Promise.all([page.waitForNavigation(), probeForm.locator('button, input[type="submit"]').first().click()]);
  const html = await page.content();
  check("the probe submitted from a browser is answered, not refused as cross-origin", !/origin/i.test(html) || /reachable|unreachable|probe/i.test(html));
  await page.screenshot({ path: join(shots, "probe-result.png"), fullPage: true });
}

// 5. Log out: the cookie goes, and /admin is the login page again.
// Logout is a POST (a GET that ended a session could be triggered by an <img>).
await page.request.post(`${base}/admin/logout`).catch(() => {});
await page.goto(`${base}/admin`);
check("after logout, /admin is the login page again", page.url().endsWith("/admin/login"), page.url());

await browser.close();
console.log(`screenshots: ${shots}`);
if (failures.length) { console.log(`ui-smoke FAILED: ${failures.join(", ")}`); process.exit(1); }
console.log("ui-smoke green");
