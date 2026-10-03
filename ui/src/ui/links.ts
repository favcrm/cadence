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
export function isLoopbackHref(rawHref: string): boolean {
  const href = rawHref.trim();
  // react-markdown hands the href percent-encoded (`http://%5B::1%5D/`),
  // which URL rejects; decode only the bracket escapes of an absolute href.
  const absolute = /^([a-z][a-z0-9+.-]*:|\/\/)/i.test(href);
  const candidate = absolute ? href.replace(/%5b/gi, "[").replace(/%5d/gi, "]") : href;
  let url: URL;
  try {
    // The fixed non-loopback base exists only so a network-path `//host/x`
    // href is judged by its host. A scheme'd href is always parsed absolute:
    // on an https board a browser treats `http:/host` as absolute.
    url = new URL(candidate, candidate.startsWith("//") ? "http://base.invalid" : undefined);
  } catch {
    // An http(s) or network-path href that will not parse even against the
    // base fails CLOSED (CAD-1075). Only a genuinely relative href is this
    // board's own origin.
    return /^(https?:|\/\/)/i.test(href);
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return false;
  const host = url.hostname.toLowerCase().replace(/^\[|\]$/g, "").replace(/\.$/, "");
  if (host === "localhost" || host.endsWith(".localhost")) return true;
  if (host === "::1" || host === "::" || host === "0:0:0:0:0:0:0:1") return true;
  // URL always emits IPv4-mapped hosts in hex: 127.x.y.z is ::ffff:7fXX:YYYY
  // and 0.0.0.0 is ::ffff:0:0 (the dotted forms never reach this test).
  if (/^::ffff:(7f[0-9a-f]{2}:[0-9a-f]{1,4}|0:0)$/.test(host)) return true;
  // The URL parser normalises 2130706433, 0x7f.1 and friends to dotted form.
  if (/^127\.\d+\.\d+\.\d+$/.test(host) || host === "0.0.0.0") return true;
  return false;
}
