import { chromium } from "playwright-core";
const base = "http://cadence-3188.localhost:3188";
const login = process.argv[2];
const browser = await chromium.launch({ headless: true, channel: "chrome",
  args: ["--host-resolver-rules=MAP *.localhost 127.0.0.1"] });
const page = await (await browser.newContext()).newPage();
page.on("pageerror", e => console.log("[pageerror]", e.message));
page.on("console", m => console.log("[console]", m.text()));
await page.goto(login);
await page.locator('section[aria-live="polite"]', { hasText: "Signed in" }).waitFor({ timeout: 30000 });
await page.goto(base);
const box = page.getByLabel("message to the master");
await box.waitFor({ timeout: 30000 });
await box.fill("/");
await page.waitForTimeout(1200);
console.log("value:", JSON.stringify(await box.inputValue()));
console.log("slashmenu:", await page.locator(".slashmenu").count());
console.log("textarea aria-expanded:", await box.getAttribute("aria-expanded"));
await browser.close();
