import { isLoopbackHref } from "../src/ui/links";

function equal(actual: unknown, expected: unknown, what: string): void {
  if (actual !== expected) throw new Error(`${what}: expected ${String(expected)}, got ${String(actual)}`);
}

// CAD-313: links to this machine are never clickable in the board.
for (const href of [
  "http://cadence-3141.localhost:3143/preview",
  "http://localhost:5173/",
  "https://LOCALHOST/x",
  "http://foo.localhost./",
  "http://127.0.0.1:3010/api",
  "http://127.1.2.3/",
  "http://2130706433/",
  "http://0x7f.1/",
  "http://0.0.0.0:8080/",
  "http://[::1]:3000/",
  "http://[::ffff:127.0.0.1]/",
  // CAD-1075: react-markdown percent-encodes the brackets; unparseable
  // http(s) fails closed.
  "http://%5B::1%5D/",
  "http://%5b::1%5d:3010/x",
  "http://[::1]/",
  "http://[0:0:0:0:0:0:0:1]/",
  "http://%5B::ffff:7f00:1%5D/",
  "http://%5Bzz%5D/",
  "//%5B::1%5D/x",
  "//[::1]/x",
  "http://%5B::ffff:0:0%5D/",
  "http://[::ffff:0.0.0.0]/",
  "http://[::ffff:0:0]:3138/",
  "http://[::ffff:127.255.1.2]/",
  " http://%5B::1%5D/",
  "\thttp://[::1]/",
]) {
  equal(isLoopbackHref(href), true, href);
}
for (const href of [
  "https://github.com/favcrm/cadence/pull/249",
  "https://example.com/localhost",
  "http://notlocalhost.com/",
  "http://10.0.0.1/",
  "/projects/cadence",
  "issue:CAD-1",
  "mailto:someone@example.com",
  "#frag",
  "?q=1",
  "https://example.com",
  "http://[2001:db8::1]/",
  "http://%5B2001:db8::1%5D/",
  "http://%5b2001:db8::1%5d/",
  "//example.com/x",
  "http://[::ffff:8.8.8.8]/",
  " https://example.com/",
]) {
  equal(isLoopbackHref(href), false, href);
}

console.log("loopback link checks passed");
