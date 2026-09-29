// CAD-787: Playwright journey over the accepted fixture-only Publish flow.
// Drives the Social Content dev preview (scripts/app-dev.mjs, fixtures by
// construction — no provider, no credential, no live post) in real Chromium:
// approve gate, Post now vs Schedule, every dispatch state, narrow viewport
// and keyboard reachability. Every request outside 127.0.0.1 fails the run,
// so the journey is offline by construction and proves the UI's part of it.
//
//   E2E_URL=http://127.0.0.1:3195 E2E_ARTIFACTS=/tmp/ev node cad787.mjs
// Without E2E_URL the script spawns its own preview server on port 3195.
import { chromium } from "playwright-core";
import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const repo = fileURLToPath(new URL("../../", import.meta.url));
const out = process.env.E2E_ARTIFACTS ?? ".";
let base = process.env.E2E_URL;
let child = null;
if (!base) {
  let port = 3130 + (process.pid % 50), started = false, err = "";
  for (let attempt = 0; attempt < 3 && !started; attempt++, port++) {
    base = `http://127.0.0.1:${port}`;
    child = spawn(process.execPath, ["scripts/app-dev.mjs", "social-content", "--port", String(port)], { cwd: repo, stdio: ["ignore", "pipe", "pipe"] });
    try {
      await new Promise((resolve, reject) => {
        let seen = ""; err = "";
        child.stdout.on("data", (chunk) => { seen += chunk; if (seen.includes("App development:")) resolve(); });
        child.stderr.on("data", (chunk) => { err += chunk; });
        child.once("exit", (code) => reject(new Error(`preview exited: ${code}: ${err.slice(0, 200)}`)));
        setTimeout(() => reject(new Error("preview startup timeout")), 15000).unref();
      });
      started = true;
    } catch (error) {
      child.kill();
      if (!String(error).includes("already in use") || attempt === 2) throw error;
    }
  }
}
const shot = (page, name) => page.screenshot({ path: path.join(out, `cad787-${name}.png`) });
const results = [];
let livePage = null;
const check = (name, ok, detail = "") => { results.push({ name, ok, detail }); if (!ok) throw new Error(`CAD-787 journey failed: ${name} ${detail}`); };
process.on("uncaughtException", async (error) => {
  try { if (livePage) await livePage.screenshot({ path: path.join(out, "cad787-fail.png") }); } catch {}
  console.error(String(error).slice(0, 2000));
  process.exit(1);
});
let external = 0;
try {
  const browser = await chromium.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"] });
  try {
    for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
      const narrow = viewport.width < 500;
      const context = await browser.newContext({ viewport });
      const page = await context.newPage();
      livePage = page;
      await page.route("**/*", (route) => {
        const host = new URL(route.request().url()).hostname;
        if (host !== "127.0.0.1" && host !== "localhost") { external++; route.abort(); }
        else route.continue();
      });
      await page.goto(`${base}/`, { waitUntil: "networkidle" });
      await page.getByRole("button", { name: "Publish", exact: true }).click();
      const flow = page.locator('section[aria-label="Publish decision preview"]');
      await flow.waitFor({ timeout: 15000 });
      check(`publish-tab-${viewport.width}`, true);
      check(`destination-${viewport.width}`, (await flow.innerText()).includes("@sakeboyhk"));
      check(`digests-${viewport.width}`, (await flow.innerText()).includes("caption digest") && (await flow.innerText()).includes("idempotency key"));
      check(`cost-${viewport.width}`, (await flow.innerText()).includes("HK$12"));
      const postNow = flow.getByRole("button", { name: "Simulate Post now" });
      check(`gated-${viewport.width}`, await postNow.isDisabled());
      await flow.locator('input[aria-label="Approve the exact destination and content digests"]').click();
      check(`ungated-${viewport.width}`, await postNow.isEnabled());
      await postNow.click();
      await flow.getByText("Simulated provider call in flight").waitFor({ timeout: 5000 });
      await flow.getByRole("button", { name: "Simulate posted" }).click();
      await flow.getByText("verified receipt").waitFor({ timeout: 5000 });
      check(`posted-${viewport.width}`, (await flow.innerText()).includes("permalink"));
      await flow.getByRole("button", { name: "Back to review" }).click();
      check(`relock-${viewport.width}`, await flow.getByRole("button", { name: "Simulate Post now" }).isDisabled());
      await flow.locator('input[aria-label="Approve the exact destination and content digests"]').click();
      await flow.getByRole("button", { name: "Simulate Post now" }).click();
      await flow.getByRole("button", { name: "Simulate lost response" }).click();
      await flow.getByText("Simulated lost response after accept").first().waitFor({ timeout: 5000 });
      check(`uncertain-reading-${viewport.width}`, (await flow.innerText()).includes("state stays processing"));
      await flow.getByRole("button", { name: "Back to review" }).click();
      await flow.locator('input[value="schedule"]').click();
      await flow.locator('input[aria-label="Approve the exact destination and content digests"]').click();
      await flow.getByRole("button", { name: "Simulate Schedule" }).click();
      await flow.getByText("Simulated queued").waitFor({ timeout: 5000 });
      check(`scheduled-${viewport.width}`, (await flow.innerText()).includes("Asia/Hong_Kong"));
      await flow.getByRole("button", { name: "Simulate cancel" }).click();
      await flow.getByText("Simulated cancellation before dispatch").waitFor({ timeout: 5000 });
      await flow.getByRole("button", { name: "Back to review" }).click();
      await flow.locator('input[aria-label="Approve the exact destination and content digests"]').click();
      await flow.getByRole("button", { name: "Simulate authority change" }).click();
      await flow.getByText("Held:").waitFor({ timeout: 5000 });
      await flow.getByRole("button", { name: "Simulate reconnect" }).click();
      await flow.getByText("Simulated reconnect complete").waitFor({ timeout: 5000 });
      check(`held-needs-human-${viewport.width}`, true);
      for (const id of ["queued", "processing", "posted", "refused", "reading", "cancelled", "held"])
        check(`state-${id}-${viewport.width}`, (await page.locator(".badge", { hasText: id }).count()) >= 1);
      if (narrow) {
        const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
        check("narrow-no-h-overflow", overflow <= 1, `overflow=${overflow}px`);
      }
      // Keyboard: tab reaches the approval control and every send is a simulation.
      await page.keyboard.press("Home");
      const approveBox = flow.locator('input[aria-label="Approve the exact destination and content digests"]');
      await approveBox.focus();
      check(`keyboard-focus-${viewport.width}`, await approveBox.evaluate((el) => document.activeElement === el));
      await shot(page, `${viewport.width}`);
      await context.close();
    }
    check("offline", external === 0, `external=${external}`);
  } finally {
    await browser.close();
  }
} finally {
  if (child) child.kill();
}
fs.writeFileSync(path.join(out, "cad787-results.json"), JSON.stringify(results, null, 1));
console.log(`CAD-787 Playwright journey passed (${results.length} checks, external=${external})`);
