/**
 * Links in agent-written markdown that point at THIS machine (CAD-313,
 * review round 2 of PR #249). A worker could post
 * `http://cadence-<board port>.localhost:<its port>/…`: cookies ignore
 * ports, so following it hands the board's session cookie to the
 * worker's listener. The cookie alone is no session any more (the page
 * key is required too), but a loopback link in the board is never
 * clickable — defence in depth. Pure, so tests/links.test.ts runs it in
 * plain node.
 */
export function isLoopbackHref(href: string): boolean {
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return false; // relative: this board's own origin
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return false;
  const host = url.hostname.toLowerCase().replace(/^\[|\]$/g, "").replace(/\.$/, "");
  if (host === "localhost" || host.endsWith(".localhost")) return true;
  if (host === "::1" || host === "::" || host === "0:0:0:0:0:0:0:1") return true;
  if (/^::ffff:(127\.|0\.0\.0\.0|7f)/.test(host)) return true;
  // The URL parser normalises 2130706433, 0x7f.1 and friends to dotted form.
  if (/^127\.\d+\.\d+\.\d+$/.test(host) || host === "0.0.0.0") return true;
  return false;
}
