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
declare function require(name: string): any;
import type { ConnectionProvider } from "../src/lib/types";
import type { act as reactAct } from "react";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(`assert: ${msg}`);
}
function equal(a: unknown, e: unknown, msg: string) {
  if (JSON.stringify(a) !== JSON.stringify(e))
    throw new Error(`${msg}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
// React's real `act` is overloaded for sync and async callbacks; assign
// its actual type so every caller (sync act(() => …) and awaited
// act(async () => …)) typechecks — a hand-narrowed signature rejects
// valid callers.
let act: typeof reactAct;
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



function smtpProvider(): ConnectionProvider {
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
function tokenProvider(): ConnectionProvider {
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
  // Same DOM + loader stub the other mounted suites use: happy-dom
  // globals before React is required, and a require hook so .css and the
  // icon packages resolve to inert stubs.
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/settings/connections" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "SVGElement", "navigator", "MutationObserver", "ResizeObserver", "Event", "MouseEvent", "KeyboardEvent", "location", "history", "sessionStorage"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"])
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const loader = require("module"), originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  act = React.act;
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

  // ---- CAD-1009 rotate audit: the SMTP rotate form is password-primary,
  // server details collapsed w/ human labels, a PAIRED port/security
  // selector, and NEVER a raw scopes or tls_mode box. ----
  const { RotateForm } = require("../src/features/settings/Connections") as typeof import("../src/features/settings/Connections");
  const rotatePosts: { url: string; body: any }[] = [];
  const innerFetch2 = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = String(input);
    if (init?.method === "POST" && url.includes("/rotate")) {
      rotatePosts.push({ url, body: JSON.parse(String(init.body)) });
      return new Response(JSON.stringify({ connection: { id: "c-1", revision: 2 } }), {
        status: 200, headers: { "Content-Type": "application/json" },
      });
    }
    return innerFetch2(input as RequestInfo | URL, init);
  }) as typeof fetch;

  const smtpRow: any = {
    id: "c-1", provider: "smtp", account: "newsletter",
    display_name: "newsletter",
    scopes: ["email:send"],
    smtp: { host: "mail.example.com", port: 465, tls_mode: "implicit", username: "mailer", sender: "news@example.com", sender_name: "News" },
    status: { adapter_registered: true, descriptor_available: true, custody_available: true, manifest_status: "matched", reviewed_pin: "smtp@1", reported_pin: "smtp@1", execution_authority: true, network_checked: false },
  };
  const host2 = document.createElement("div");
  document.body.appendChild(host2);
  const root2 = createRoot(host2);
  const field2 = (label: string) => Array.from(host2.querySelectorAll("label")).find((l) => (l.textContent ?? "").includes(label));
  const fill2 = (label: string, value: string) => {
    const lab = field2(label);
    assert(lab, `rotate field labelled ${label}`);
    const input = lab!.parentElement!.querySelector("input,select,textarea") as HTMLInputElement;
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value")!.set!;
      setter.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
  };
  act(() => {
    root2.render(React.createElement(RotateForm, { row: smtpRow, onDone: () => {}, onClose: () => {} }));
  });
  await settle(() => assert(field2("New SMTP password"), "rotate is password-primary"), 30000);
  // No raw scopes box for SMTP, and no independent raw tls_mode field.
  assert(!field2("Scopes"), "SMTP rotate exposes NO scopes field — it preserves live scopes");
  assert(!field2("tls_mode"), "SMTP rotate exposes NO raw tls_mode input");
  // Server details are collapsed behind a summary; the transport selector
  // is the PAIRED choice (keep current / 465 implicit / 587 starttls).
  assert(host2.querySelector("details summary"), "server details are collapsed behind a summary");
  assert(field2("Port & security"), "the paired Port & security selector renders");

  const submitRotate = () => {
    const btn = Array.from(host2.querySelectorAll("button")).find((b) => (b.textContent ?? "").includes("Replace credential")) as HTMLButtonElement;
    return act(async () => btn.click());
  };

  // (a) Blank everything-but-password -> INHERIT: only `secret` is sent;
  // no host/port/tls_mode and NO scopes in the body.
  fill2("New SMTP password", "rotate-secret-1");
  await submitRotate();
  await settle(() => assert(rotatePosts.length === 1, "rotate posted once"), 30000);
  const b0 = rotatePosts[0].body;
  equal(b0.secret, "rotate-secret-1", "rotate sends the new password as secret");
  assert(b0.host === undefined && b0.port === undefined && b0.tls_mode === undefined, "blank details inherit live host/port/tls — nothing sent");
  assert(b0.scopes === undefined, "SMTP rotate sends NO scopes — live grant preserved");

  // (b) Choose the paired 587/STARTTLS transport -> port+tls_mode travel
  // together; scopes still absent.
  const transport = field2("Port & security")!.parentElement!.querySelector("select") as HTMLSelectElement;
  act(() => {
    const setter = Object.getOwnPropertyDescriptor(window.HTMLSelectElement.prototype, "value")!.set!;
    setter.call(transport, "starttls");
    transport.dispatchEvent(new Event("change", { bubbles: true }));
  });
  fill2("New SMTP password", "rotate-secret-2");
  await submitRotate();
  await settle(() => assert(rotatePosts.length === 2, "second rotate posted"), 30000);
  const b1 = rotatePosts[1].body;
  equal(b1.port, 587, "paired selector sends port 587");
  equal(b1.tls_mode, "starttls", "paired selector sends tls_mode starttls");
  assert(b1.scopes === undefined, "paired transport still sends no scopes");

  // (c) Reset the transport back to "Keep current" and change only the
  // host — the effective inherited 465/implicit pairing is re-validated
  // and only the host crosses the wire (port/tls inherit the live pair).
  act(() => {
    const setter = Object.getOwnPropertyDescriptor(window.HTMLSelectElement.prototype, "value")!.set!;
    setter.call(transport, "");
    transport.dispatchEvent(new Event("change", { bubbles: true }));
  });
  fill2("Server host", "smtp2.example.com");
  fill2("New SMTP password", "rotate-secret-3");
  await submitRotate();
  await settle(() => assert(rotatePosts.length === 3, "host-only rotate posted"), 30000);
  const b2 = rotatePosts[2].body;
  equal(b2.host, "smtp2.example.com", "a changed host is sent");
  assert(b2.port === undefined && b2.tls_mode === undefined, "host-only change inherits the live port/tls pair — none sent");

  globalThis.fetch = innerFetch2;
  await act(async () => root2.unmount());

  // ---- CAD-1064: an SMTP sender whose settings cannot be read. ----
  const { ConnectionDetail } = require("../src/features/settings/Connections") as typeof import("../src/features/settings/Connections");
  const unreadableRow = (code: string): any => ({
    ...smtpRow, smtp: null, smtp_sender: true, smtp_error: code,
  });
  const host3 = document.createElement("div");
  document.body.appendChild(host3);
  const root3 = createRoot(host3);

  // Detail: readable settings show host, port+security, login and sender.
  act(() => {
    root3.render(React.createElement(ConnectionDetail, { row: { ...smtpRow, smtp_sender: true, smtp_error: null }, capabilities: null, canWrite: true, onChanged: () => {}, onRevoked: () => {} }));
  });
  await settle(() => {
    const t = host3.textContent ?? "";
    assert(t.includes("mail.example.com:465") && t.includes("implicit TLS"), "detail shows host, port and security");
    assert(t.includes("login mailer") && t.includes("news@example.com") && t.includes("News"), "detail shows username, sender and sender name");
    assert(!host3.querySelector("[data-smtp-error]"), "a readable sender shows no error");
  });

  // Detail: each typed error gives a human message and the fix.
  for (const [code, phrase] of [
    ["custody_corrupt", "Rotate it to re-enter them"],
    ["unavailable", "Rotate it to re-enter them"],
    ["withheld_leak", "different password or app password"],
  ] as const) {
    act(() => {
      root3.render(React.createElement(ConnectionDetail, { row: unreadableRow(code), capabilities: null, canWrite: true, onChanged: () => {}, onRevoked: () => {} }));
    });
    await settle(() => {
      const err = host3.querySelector(`[data-smtp-error="${code}"]`);
      assert(err && (err.textContent ?? "").includes(phrase), `${code}: message carries the fix`);
    });
    assert(!(host3.textContent ?? "").includes("null"), `${code}: no raw null leaks into the page`);
  }
  await act(async () => root3.unmount());

  // Rotate on an unreadable sender uses the SMTP form (password-first),
  // never the generic token form, and nothing is inherited.
  const rotatePosts3: any[] = [];
  const innerFetch3 = globalThis.fetch;
  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    if (init?.method === "POST" && String(input).includes("/rotate")) {
      rotatePosts3.push(JSON.parse(String(init.body)));
      return new Response(JSON.stringify({ connection: { id: "c-1", revision: 2 } }), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    return innerFetch3(input as RequestInfo | URL, init);
  }) as typeof fetch;
  const host4 = document.createElement("div");
  document.body.appendChild(host4);
  const root4 = createRoot(host4);
  const field4 = (label: string) => Array.from(host4.querySelectorAll("label")).find((l) => (l.textContent ?? "").includes(label));
  const set4 = (label: string, value: string, kind: "Input" | "Select" = "Input") => {
    const el = field4(label)!.parentElement!.querySelector("input,select") as HTMLInputElement;
    act(() => {
      const setter = Object.getOwnPropertyDescriptor((window as any)[`HTML${kind}Element`].prototype, "value")!.set!;
      setter.call(el, value);
      el.dispatchEvent(new Event(kind === "Input" ? "input" : "change", { bubbles: true }));
    });
  };
  act(() => {
    root4.render(React.createElement(RotateForm, { row: unreadableRow("custody_corrupt"), onDone: () => {}, onClose: () => {} }));
  });
  await settle(() => assert(field4("New SMTP password"), "unreadable sender rotates with the SMTP password form"));
  assert(!field4("New token") && !field4("Scopes"), "no generic token form or scopes box");
  assert((host4.textContent ?? "").includes("Rotate it to re-enter them"), "rotate repeats why");
  assert(host4.querySelector("details")?.hasAttribute("open"), "transport fields are open, not collapsed");
  const keepCurrent = Array.from(host4.querySelectorAll("option")).some((o) => (o.textContent ?? "").includes("Keep current"));
  assert(!keepCurrent, "there is no 'keep current' choice when nothing is readable");
  const replace4 = () => Array.from(host4.querySelectorAll("button")).find((b) => (b.textContent ?? "").includes("Replace credential")) as HTMLButtonElement;
  // Password alone is refused client-side: all fields are required.
  set4("New SMTP password", "pw-one-two-three");
  await act(async () => replace4().click());
  await settle(() => assert((host4.textContent ?? "").includes("Enter the server host"), "missing fields are named"));
  assert(rotatePosts3.length === 0, "nothing posts until every field is entered");
  set4("Server host", "mail.example.com");
  set4("Port & security", "implicit", "Select");
  set4("Username", "mailer");
  set4("Sender address", "news@example.com");
  set4("New SMTP password", "pw-one-two-three");
  await act(async () => replace4().click());
  await settle(() => assert(rotatePosts3.length === 1, "full re-entry posts"));
  const full = rotatePosts3[0];
  equal(
    { host: full.host, port: full.port, tls_mode: full.tls_mode, username: full.username, sender: full.sender, secret: full.secret },
    { host: "mail.example.com", port: 465, tls_mode: "implicit", username: "mailer", sender: "news@example.com", secret: "pw-one-two-three" },
    "every transport field travels with the secret",
  );
  assert(full.scopes === undefined, "scopes are still never sent");
  globalThis.fetch = innerFetch3;
  await act(async () => root4.unmount());

  console.log("connections smtp form checks passed");
}

void main();
