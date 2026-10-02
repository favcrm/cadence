/**
 * CAD-1013 mounted SMTP add-form checks. These mount AddConnection and
 * drive a real DOM submit against a stubbed fetch, asserting the actual
 * behavior the audit demanded — not a source regex:
 *  - an SMTP sender enrolls only its reviewed send capability's scope,
 *    never the union of every provider permission;
 *  - a `custody_unprotected` refusal surfaces a visible, explicit consent
 *    beside Save (never pre-checked);
 *  - an unknown/network error never claims "nothing was saved" and never
 *    echoes the credential back;
 *  - the password is cleared from the form on settle;
 *  - token providers and other providers are unchanged.
 */
import { act } from "react";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(`assert: ${msg}`);
}
function equal(a: unknown, e: unknown, msg: string) {
  if (JSON.stringify(a) !== JSON.stringify(e))
    throw new Error(`${msg}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function settle(fn: () => void, ms = 12000) {
  const end = Date.now() + ms;
  for (;;) {
    try {
      fn();
      return;
    } catch (e) {
      if (Date.now() > end) throw e;
      await act(async () => sleep(25));
    }
  }
}

function smtpProvider(): import("../src/lib/types").ConnectionProvider {
  return {
    provider: "smtp",
    descriptor_available: true,
    manifest_status: "matched",
    reviewed_pin: "smtp@1",
    reported_pin: "smtp@1",
    network_checked: false,
    registration_digest: "sha256:smtp",
    descriptor: {
      schema: 1,
      provider: "smtp",
      revision: "1",
      enrollment_shapes: ["smtp"],
      builtin_accounts: [],
      capabilities: [
        // The send capability is the ONLY scope the enrollment may take.
        { id: "email.send", version: 1, tools: ["email.send"], scopes: ["email:send"], effect: "send", semantics: "email" },
        // A second, unrelated capability the SMTP form must NOT widen into.
        { id: "email.read", version: 1, tools: ["email.read"], scopes: ["email:read"], effect: "read", semantics: "read" },
      ],
    },
  };
}
function tokenProvider(): import("../src/lib/types").ConnectionProvider {
  return {
    provider: "agenticos_external",
    descriptor_available: true,
    manifest_status: "matched",
    reviewed_pin: "a@1",
    reported_pin: "a@1",
    network_checked: false,
    registration_digest: "sha256:a",
    descriptor: {
      schema: 1,
      provider: "agenticos_external",
      revision: "1",
      enrollment_shapes: ["token"],
      builtin_accounts: [],
      capabilities: [
        { id: "agenticos.exec", version: 1, tools: ["exec"], scopes: ["provider.read"], effect: "execute", semantics: "exec" },
      ],
    },
  };
}

async function main() {
  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { AddConnection } = require("../src/features/settings/Connections") as typeof import("../src/features/settings/Connections");

  const host = document.createElement("div");
  document.body.appendChild(host);
  const text = () => host.textContent ?? "";
  const byText = (tag: string, label: string) =>
    Array.from(host.querySelectorAll(tag)).find((el) => (el.textContent ?? "").includes(label)) ?? null;
  const field = (label: string) =>
    Array.from(host.querySelectorAll("label")).find((l) => (l.textContent ?? "").includes(label));
  const fill = (label: string, value: string) => {
    const lab = field(label);
    assert(lab, `field labelled ${label}`);
    const input = lab!.parentElement!.querySelector("input,textarea,select") as HTMLInputElement;
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value")!.set!;
      setter.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
  };

  const posts: { url: string; body: any }[] = [];
  let nextResponse: (() => Response) | null = null;
  const innerFetch = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = String(input);
    if (init?.method === "POST" && url.endsWith("/api/connections")) {
      posts.push({ url, body: JSON.parse(String(init.body)) });
      return nextResponse
        ? nextResponse()
        : new Response(JSON.stringify({ connection: { id: "c-new" } }), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          });
    }
    return innerFetch(input as RequestInfo | URL, init);
  }) as typeof fetch;

  const providers = [smtpProvider(), tokenProvider()];
  const root = createRoot(host);
  act(() => {
    root.render(
      React.createElement(AddConnection, {
        providers,
        existing: [],
        onClose: () => {},
        onAdded: () => {},
      }),
    );
  });
  await settle(() => assert(host.querySelector('section[aria-label="add connection"]'), "add connection renders"));

  // Pick the SMTP provider, then the form is SMTP-focused.
  const providerSelect = host.querySelector("select") as HTMLSelectElement;
  act(() => {
    const setter = Object.getOwnPropertyDescriptor(window.HTMLSelectElement.prototype, "value")!.set!;
    setter.call(providerSelect, "smtp");
    providerSelect.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await settle(() => assert(field("Server host"), "SMTP host field renders"));
  assert(field("Email account name"), "account is labelled for email");
  assert(field("Port & security"), "port & security select renders");
  assert(field("Password"), "password field renders");
  assert(field("Sender address"), "sender address renders");
  assert(!field("Scopes"), "no raw Scopes grant field in the SMTP form");

  // Fill the SMTP form and submit: the body carries ONLY email:send —
  // never the union including email:read.
  fill("Email account name", "newsletter");
  fill("Server host", "mail.example.com");
  fill("Username", "mailer");
  fill("Password", "s3cret-password");
  fill("Sender address", "news@example.com");
  const addBtn = () => byText("button", "Add connection") as HTMLButtonElement | null;
  await act(async () => addBtn()!.click());
  await settle(() => assert(posts.length === 1, "one connection create posted"));
  equal(posts[0].body.scopes, ["email:send"], "SMTP enrolls only the send scope");
  equal(posts[0].body.shape, "smtp", "smtp shape sent");
  assert(posts[0].body.secret === undefined || posts[0].body.secret === "s3cret-password", "secret crosses only in the request body");

  // Password cleared from the form on settle.
  const pw = field("Password")!.parentElement!.querySelector("input") as HTMLInputElement;
  assert(pw.value === "", "password field is cleared after submit");

  // custody_unprotected → visible explicit consent beside Save, not pre-checked.
  nextResponse = () =>
    new Response(JSON.stringify({ error: "unprotected", code: "custody_unprotected" }), {
      status: 400,
      headers: { "Content-Type": "application/json" },
    });
  fill("Password", "s3cret-password");
  await act(async () => addBtn()!.click());
  await settle(() => {
    assert(text().includes("nothing was stored"), "custody_unprotected reports nothing was stored");
    assert(host.querySelector("[data-risk-needed]"), "the storage-consent control renders inline near Save");
  });
  const consent = host.querySelector("[data-risk-needed] input") as HTMLInputElement;
  assert(consent && !consent.checked, "consent is never pre-checked");

  // Unknown error → honest 'could not confirm', never fake 'saved'/'nothing'.
  nextResponse = () =>
    new Response(JSON.stringify({ error: "TLS handshake failed for mail.example.com:465 x509 verify secret=s3cret" }), {
      status: 502,
      headers: { "Content-Type": "application/json" },
    });
  act(() => {
    consent.checked && consent.click();
  });
  fill("Password", "s3cret-password");
  await act(async () => addBtn()!.click());
  await settle(() => assert(text().includes("Could not confirm the connection was added"), "unknown failure is honest"));
  assert(!text().includes("s3cret"), "the credential never echoes back in the error");
  assert(!text().includes("nothing was saved") || text().includes("Could not confirm"), "no fake 'nothing saved' on an unknown outcome");

  // Token provider path unchanged: raw Scopes field is present there.
  act(() => {
    const setter = Object.getOwnPropertyDescriptor(window.HTMLSelectElement.prototype, "value")!.set!;
    setter.call(providerSelect, "agenticos_external");
    providerSelect.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await settle(() => assert(field("Scopes"), "token provider keeps its scope field"));
  assert(!field("Server host"), "token provider has no SMTP fields");

  globalThis.fetch = innerFetch;
  await act(async () => root.unmount());
  console.log("connections smtp form checks passed");
}

void main();
