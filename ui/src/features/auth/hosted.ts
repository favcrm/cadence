/**
 * The signed-out gate paints before `GET /api/meta` answers, and a failed check
 * may never answer at all (503, timeout, offline). On a hosted board the
 * local-operator copy ("Private board", `cadence ui login`) is never true, so
 * the gate needs a hosted/local decision the server cannot supply yet.
 *
 * Loopback, private-network and `.local`/`.localhost` names are a local board;
 * everything else is hosted. A successful meta answer still replaces this hint.
 */
const LOCAL_HOST =
  /^(?:localhost|127(?:\.\d{1,3}){3}|\[::1\]|::1|0\.0\.0\.0|10(?:\.\d{1,3}){3}|192\.168(?:\.\d{1,3}){2}|172\.(?:1[6-9]|2\d|3[01])(?:\.\d{1,3}){2}|169\.254(?:\.\d{1,3}){2}|100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7])(?:\.\d{1,3}){2}|\[::\]|\[(?:f[cd][0-9a-f]{2}|fe[89ab][0-9a-f]):[0-9a-f:.]+\]|.*\.localhost|.*\.local|.*\.ts\.net|.*\.tailscale\.net)$/i;

function mappedIpv4(host: string): string | null {
  const dotted = /^\[::ffff:(\d{1,3}(?:\.\d{1,3}){3})\]$/i.exec(host);
  if (dotted) return dotted[1];
  const hex = /^\[::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})\]$/i.exec(host);
  if (!hex) return null;
  const high = Number.parseInt(hex[1], 16);
  const low = Number.parseInt(hex[2], 16);
  return `${high >> 8}.${high & 255}.${low >> 8}.${low & 255}`;
}

export function hostedOrigin(hostname: string | null | undefined): boolean {
  if (typeof hostname !== "string") return true;
  // FQDNs may carry the root label (`box.ts.net.`); it does not change the host.
  const host = hostname.trim().replace(/\.+$/, "");
  // No readable location (locked-down webview): a hosted answer is the safe
  // default — it offers a retry and a real sign-in rather than a CLI command.
  if (host.length === 0) return true;
  // IPv4-mapped IPv6 is the IPv4 address in another notation; classify the
  // address it maps to. A browser serializes IPv6 as hex groups
  // (`[::ffff:c0a8:105]`), while a direct string may carry the dotted form
  // (`[::ffff:192.168.1.5]`); both mean the same host.
  const mapped = mappedIpv4(host);
  if (mapped !== null) return !LOCAL_HOST.test(mapped);
  return !LOCAL_HOST.test(host);
}
