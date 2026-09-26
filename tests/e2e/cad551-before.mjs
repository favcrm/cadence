#!/usr/bin/env node
// CAD-551 "before" captures: the pre-change board (ui/dist built from
// origin/main) against the same daemon, dark + light frames of the same
// thread the "after" run exercises.
//
//   E2E_URL=http://cadence-<p>.localhost:<p> E2E_LOGIN=<link> \
//   E2E_ARTIFACTS=<dir> node cad551-before.mjs
import { chromium } from "playwright-core";
import fs from "node:fs";
import path from "node:path";

const base = process.env.E2E_URL;
const login = process.env.E2E_LOGIN;
const out = process.env.E2E_ARTIFACTS ?? ".";
if (!base || !login) {
  console.error("usage: E2E_URL=… E2E_LOGIN=… node cad551-before.mjs");
  process.exit(2);
}
fs.mkdirSync(out, { recursive: true });
const shot = (page, name) => page.screenshot({ path: path.join(out, name) });

const browser = await chromium.launch({
  headless: true,
  channel: "chrome",
  args: ["--host-resolver-rules=MAP *.localhost 127.0.0.1"],
});
const context = await browser.newContext({
  viewport: { width: 1400, height: 1000 },
  colorScheme: "dark",
});
const page = await context.newPage();
await page.goto(login);
await page.getByText("Signed in as the operator.").waitFor({ timeout: 15000 });
await page.goto(base);
// The base UI's composer placeholder differs; wait on the thread itself.
await page.getByText("fake-pi reply").first().waitFor({ timeout: 15000 });
await page.waitForTimeout(600);
await shot(page, "before-dark.png");

await page.evaluate(() => localStorage.setItem("cadence-theme", "light"));
await page.emulateMedia({ colorScheme: "light" });
await page.reload();
await page.getByText("fake-pi reply").first().waitFor({ timeout: 15000 });
await page.waitForTimeout(600);
await shot(page, "before-light.png");

await context.close();
await browser.close();
console.log("cad551-before: done");
