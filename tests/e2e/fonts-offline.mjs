// CAD-871: a screenshot must not depend on web-font loading.
//
//   cd tests/e2e && pnpm install && node fonts-offline.mjs
//   (E2E_CHROME=<binary> overrides the installed Chrome)
//
// A page declares a same-origin @font-face whose file is requested and
// then never answered, plus one on an unreachable host. The context
// carries board.mjs's routing policy (route.mjs). The screenshot gets
// a 10 s budget; before the fix it waited out the whole budget on the
// hung font. Exits 1 when the capture times out, when a font request
// reached the network, or when a font counts as an off-host reach.

import { chromium } from "playwright-core";
import http from "node:http";
import { routeContext } from "./route.mjs";

const hung = [];
const sockets = new Set();
const server = http.createServer((req, res) => {
  if (req.url === "/hung.woff2") {
    hung.push(req.url); // never answered
    return;
  }
  res.setHeader("content-type", "text/html");
  res.end(`<!doctype html><style>
    @font-face { font-family: Hung; src: url(/hung.woff2) format("woff2"); }
    @font-face { font-family: Gone; src: url(http://fonts.invalid/gone.woff2) format("woff2"); }
    body { font-family: Hung, Gone, sans-serif; }
  </style><h1>fonts offline</h1>`);
});
server.on("connection", (s) => {
  sockets.add(s);
  s.on("close", () => sockets.delete(s));
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const origin = `http://127.0.0.1:${server.address().port}`;

const launch = { headless: true };
if (process.env.E2E_CHROME) launch.executablePath = process.env.E2E_CHROME;
else launch.channel = "chrome";
const browser = await chromium.launch(launch);
let failure = null;
try {
  const context = await browser.newContext({ viewport: { width: 800, height: 600 } });
  const offsite = [];
  await routeContext(context, origin, offsite);
  const page = await context.newPage();
  await page.goto(origin, { waitUntil: "domcontentloaded" });
  await page.screenshot({ timeout: 10_000, fullPage: true });
  if (hung.length) failure = `the hung font reached the server: ${hung}`;
  else if (offsite.length) failure = `a font counted as an off-host reach: ${offsite}`;
} catch (e) {
  failure = String(e?.message ?? e);
} finally {
  await browser.close().catch(() => {});
  for (const s of sockets) s.destroy();
  server.close();
}
if (failure) {
  console.error(`FAIL: ${failure}`);
  process.exit(1);
}
console.log("ok: the screenshot completed with every font request blocked");
