import { hostedOrigin } from "../src/features/auth/hosted";

function equal(actual: unknown, expected: unknown, what: string): void {
  if (actual !== expected) throw new Error(`${what}: expected ${String(expected)}, got ${String(actual)}`);
}

// A hosted board must never be mistaken for a local one before /api/meta answers.
equal(hostedOrigin("fav-limited.cadencecloud.app"), true, "tenant host");
equal(hostedOrigin("app-v2.agenticos.hk"), true, "platform host");
equal(hostedOrigin("board.example.test"), true, "public test host");
// Local boards keep the local copy.
equal(hostedOrigin("localhost"), false, "localhost");
equal(hostedOrigin("127.0.0.1"), false, "loopback");
equal(hostedOrigin("::1"), false, "ipv6 loopback");
equal(hostedOrigin("studio.localhost"), false, "subdomain localhost");
equal(hostedOrigin("192.168.1.20"), false, "private lan");
equal(hostedOrigin("10.0.0.5"), false, "private 10/8");
equal(hostedOrigin("172.20.1.4"), false, "private 172.16/12");
equal(hostedOrigin("100.101.102.103"), false, "tailnet");
equal(hostedOrigin("box.local"), false, "mdns name");
equal(hostedOrigin("ip-172-31-1-32.tail9fcf30.ts.net"), false, "tailnet MagicDNS");
equal(hostedOrigin("host.tailscale.net"), false, "tailscale DNS");
equal(hostedOrigin("ip-172-31-1-32.tail9fcf30.ts.net."), false, "trailing root label");
equal(hostedOrigin("board.example.test."), true, "trailing root label on a public host");
// No readable location: hosted is the safer copy (retry + sign-in, no CLI).
equal(hostedOrigin(undefined), true, "no location");
equal(hostedOrigin(""), true, "empty host");

console.log("hosted-origin checks passed");
