// CAD-871: the request policy of the e2e browser context, shared by
// board.mjs and its test (fonts-offline.mjs).
//
// The journey is offline by construction: anything off the board's
// origin is aborted and recorded, and the step fails on it.
//
// Web fonts are the exception, and are never fetched at all. A
// screenshot waits for `document.fonts.ready`; in staging run
// 36784227530 that wait exceeded 30 s for a cause that is not known
// (possibly inside Chrome). Aborting every font request settles the
// load at once and the page falls back to its system fonts, which
// removes the capture's dependency on font loading. The journey
// asserts text and structure, not typefaces.

/** @param {import("playwright-core").BrowserContext} context */
export async function routeContext(context, origin, offsite) {
  await context.route("**/*", (route) => {
    const request = route.request();
    const url = request.url();
    let same = false;
    try {
      same = new URL(url).origin === origin;
    } catch {
      same = false;
    }
    if (request.resourceType() === "font") {
      // Never fetched, but a font off the board is still an off-host reach.
      if (!same && !url.startsWith("data:")) offsite.push(url);
      return route.abort();
    }
    if (same || url.startsWith("data:") || url.startsWith("blob:")) {
      return route.continue();
    }
    offsite.push(url);
    return route.abort();
  });
}
