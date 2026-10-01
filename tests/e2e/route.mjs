// CAD-871: the request policy of the e2e browser context, shared by
// board.mjs and its test (fonts-offline.mjs).
//
// The journey is offline by construction: anything off the board's
// origin is aborted and recorded, and the step fails on it.
//
// Web fonts are the exception, and are never fetched at all. A
// screenshot waits for `document.fonts.ready`, so a font request that
// is slow or never answers (a loaded runner, a stalled socket) turns
// into "page.screenshot: Timeout ... waiting for fonts to load"
// (staging run 36784227530). Aborting every font request settles the
// load at once and the page falls back to its system fonts, so the
// capture no longer depends on any font transport. The journey
// asserts text and structure, not typefaces.

/** @param {import("playwright-core").BrowserContext} context */
export async function routeContext(context, origin, offsite) {
  await context.route("**/*", (route) => {
    const request = route.request();
    if (request.resourceType() === "font") {
      return route.abort();
    }
    const url = request.url();
    let same = false;
    try {
      same = new URL(url).origin === origin;
    } catch {
      same = false;
    }
    if (same || url.startsWith("data:") || url.startsWith("blob:")) {
      return route.continue();
    }
    offsite.push(url);
    return route.abort();
  });
}
