/**
 * CAD-2578: Settings -> Security in happy-dom against a synthetic board.
 * Verifies every endpoint contract, required CSRF headers, OTP-only vs
 * password-gated 2FA setup/regenerate/disable, org-required lock, reset email,
 * reauthRequired callback, session-reset secret scrubbing, and zh-TW i18n.
 */
declare function require(name: string): any;
export {};

function ok(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg);
}

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/settings/security" });
  for (const name of [
    "window", "document", "Node", "Element", "HTMLElement", "HTMLInputElement", "HTMLButtonElement", "SVGElement",
    "navigator", "MutationObserver", "Event", "MouseEvent", "KeyboardEvent", "PointerEvent", "location", "history",
    "localStorage", "sessionStorage",
  ]) {
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  }
  for (const name of ["addEventListener", "removeEventListener", "dispatchEvent"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true, writable: true });
  }
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

  const loader = require("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    return originalRequire.apply(this, arguments);
  };

  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { act } = React;
  const { LocaleTextProvider } = require("../src/lib/localeText") as typeof import("../src/lib/localeText");
  const { translateUiText } = require("../src/lib/locale") as typeof import("../src/lib/locale");
  const { allowPrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  const Security = (require("../src/features/settings/Security") as typeof import("../src/features/settings/Security")).default;

  allowPrivateReads();

  type Call = { method: string; path: string; body: unknown; headers: Record<string, string> };
  const calls: Call[] = [];
  let routeHandler: (method: string, path: string, body: unknown) => Response = () =>
    Response.json({ ok: false, error: { code: "unhandled", message: "unhandled" } }, { status: 500 });

  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const path = String(input);
    const method = init?.method ?? "GET";
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;
    calls.push({ method, path, body, headers: { ...(init?.headers as Record<string, string> | undefined) } });
    return routeHandler(method, path, body);
  }) as typeof fetch;

  const json = (value: unknown, status = 200) =>
    new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const fail = (status: number, code: string, message = code) => json({ ok: false, error: { code, message } }, status);

  const flush = async () => {
    for (let i = 0; i < 6; i += 1) {
      await act(async () => {
        await Promise.resolve();
      });
    }
  };

  const setInput = (el: HTMLInputElement, value: string) => {
    act(() => {
      const desc = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value");
      desc?.set?.call(el, value);
      el.dispatchEvent(new win.Event("input", { bubbles: true }));
      el.dispatchEvent(new win.Event("change", { bubbles: true }));
    });
  };

  const submitForm = (form: HTMLFormElement) => {
    act(() => {
      form.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true }));
    });
  };

  const buttonByText = (root: Element, label: string) =>
    Array.from(root.querySelectorAll("button")).find((b) => b.textContent?.trim() === label) as HTMLButtonElement | undefined;

  const mount = async (props: { onSessionInvalidated?: () => void; zh?: boolean } = {}) => {
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    const node = props.zh
      ? React.createElement(LocaleTextProvider, { value: (s: string) => translateUiText(s, "zh-TW") }, React.createElement(Security, { onSessionInvalidated: props.onSessionInvalidated }))
      : React.createElement(Security, { onSessionInvalidated: props.onSessionInvalidated });
    act(() => { root.render(node); });
    await flush();
    return {
      host,
      unmount: () => { act(() => root.unmount()); host.remove(); },
    };
  };

  // 1. Initial load failure -> Retry recovers
  {
    let attempt = 0;
    routeHandler = (method, path) => {
      ok(method === "GET" && path === "/api/account/security", "expected initial security read");
      attempt += 1;
      if (attempt === 1) return fail(403, "operator_unsupported");
      return json({
        email: "member@example.com",
        hasPassword: false,
        twoFactorEnabled: false,
        backupCodesRemaining: 0,
        requiredByWorkspaces: [],
      });
    };
    const { host, unmount } = await mount();
    ok(host.textContent?.includes("Account security settings are personal to a signed-in user."), "operator refusal mapped cleanly");
    const retry = buttonByText(host, "Retry");
    ok(retry, "Retry button present");
    act(() => { retry.click(); });
    await flush();
    ok(host.textContent?.includes("member@example.com"), "retried read displays email");
    ok(host.textContent?.includes("No password set"), "shows passwordless status");
    unmount();
  }

  // 2. OTP-only account: set password + passwordless 2FA enable/verify/backup-codes
  {
    calls.length = 0;
    const state = {
      email: "otp.user@example.com",
      hasPassword: false,
      twoFactorEnabled: false,
      backupCodesRemaining: 0,
      requiredByWorkspaces: [] as { id: string; name: string }[],
    };
    const totpURI = "otpauth://totp/AgenticOS:otp.user%40example.com?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP&issuer=AgenticOS&digits=6&period=30";
    const backupCodes = Array.from({ length: 10 }, (_, i) => `abcde-0000${i}`);

    routeHandler = (method, path, body) => {
      if (method === "GET" && path === "/api/account/security") return json(state);
      if (path === "/api/account/security/password/set") {
        state.hasPassword = true;
        return json({ hasPassword: true });
      }
      if (path === "/api/account/security/two-factor/enable") {
        return json({ totpURI, backupCodes });
      }
      if (path === "/api/account/security/two-factor/verify") {
        const b = body as { code?: string };
        if (b.code !== "654321") return fail(400, "invalid_totp_code");
        state.twoFactorEnabled = true;
        state.backupCodesRemaining = 10;
        return json({ twoFactorEnabled: true });
      }
      return fail(404, "not_found");
    };

    const { host, unmount } = await mount();

    // Enable 2FA directly without password prompt because !hasPassword
    const enableBtn = buttonByText(host, "Enable two-factor authentication");
    ok(enableBtn, "Enable 2FA button rendered");
    act(() => { enableBtn.click(); });
    await flush();

    const enableCall = calls.find((c) => c.path === "/api/account/security/two-factor/enable");
    ok(enableCall, "enable called directly");
    equal(enableCall.body, {}, "no password sent for OTP-only account");
    ok(enableCall.headers["X-Cadence-Board"] === "1" && enableCall.headers["Content-Type"] === "application/json", "POST security headers enforced");

    ok(host.querySelector('svg[role="img"][aria-label="Authenticator QR code"] path'), "QR SVG rendered");
    ok(host.textContent?.includes("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP"), "manual setup key shown");

    // Wrong verification code shows friendly error
    const codeInput = host.querySelector('input[inputmode="numeric"]') as HTMLInputElement;
    ok(codeInput, "6-digit input rendered");
    setInput(codeInput, "000000");
    submitForm(codeInput.closest("form")!);
    await flush();
    ok(host.textContent?.includes("That verification code didn’t match"), "invalid totp mapped");

    // Valid code verifies and shows 10 backup codes
    setInput(codeInput, "654321");
    submitForm(codeInput.closest("form")!);
    await flush();
    ok(host.textContent?.includes("abcde-00000") && host.textContent?.includes("Save your backup codes"), "backup codes revealed");
    ok(host.textContent?.includes("10 backup codes remaining"), "remaining count updated");

    // Set password -> mismatch guard -> success
    const setPwToggle = buttonByText(host, "Set password");
    ok(setPwToggle, "Set password toggle rendered");
    act(() => { setPwToggle.click(); });
    await flush();
    const inputs = host.querySelectorAll('input[type="password"]');
    equal(inputs.length, 2, "new + confirm password inputs");
    setInput(inputs[0] as HTMLInputElement, "strong-pass-123");
    setInput(inputs[1] as HTMLInputElement, "mismatch-pass");
    submitForm((inputs[0] as HTMLInputElement).closest("form")!);
    await flush();
    ok(host.textContent?.includes("Passwords do not match."), "client mismatch validated");

    setInput(inputs[1] as HTMLInputElement, "strong-pass-123");
    submitForm((inputs[0] as HTMLInputElement).closest("form")!);
    await flush();
    const setCall = calls.find((c) => c.path === "/api/account/security/password/set");
    equal(setCall?.body, { newPassword: "strong-pass-123" }, "set password payload exact");
    ok(host.textContent?.includes("Password set"), "chip flips to Password set");

    unmount();
  }

  // 3. Password user: change password + forgot reset email + reauthRequired + regen/disable + org policy lock
  {
    calls.length = 0;
    let invalidated = 0;
    const state = {
      email: "owner@example.com",
      hasPassword: true,
      twoFactorEnabled: true,
      backupCodesRemaining: 10,
      requiredByWorkspaces: [] as { id: string; name: string }[],
    };
    const regenCodes = Array.from({ length: 10 }, (_, i) => `zzzzz-1000${i}`);

    routeHandler = (method, path, body) => {
      if (method === "GET" && path === "/api/account/security") return json(state);
      if (path === "/api/account/security/password/reset-email") {
        return json({ sent: true, email: state.email });
      }
      if (path === "/api/account/security/password/change") {
        return json({ hasPassword: true, revokedOtherBoardSessions: 2, reauthRequired: true });
      }
      if (path === "/api/account/security/two-factor/backup-codes") {
        const b = body as { password?: string };
        if (b.password !== "pw-ok-123") return fail(400, "invalid_password");
        return json({ backupCodes: regenCodes });
      }
      if (path === "/api/account/security/two-factor/disable") {
        state.twoFactorEnabled = false;
        state.backupCodesRemaining = 0;
        return json({ twoFactorEnabled: false });
      }
      return fail(404, "not_found");
    };

    const { host, unmount } = await mount({ onSessionInvalidated: () => { invalidated += 1; } });

    // Open Change password & click Forgot password?
    act(() => { buttonByText(host, "Change password")!.click(); });
    await flush();
    act(() => { buttonByText(host, "Forgot password?")!.click(); });
    await flush();
    ok(host.textContent?.includes("Reset link sent to owner@example.com."), "inline reset email sent feedback");

    // Submit Change password with default revokeOtherSessions = true
    const pwInputs = host.querySelectorAll('input[type="password"]');
    equal(pwInputs.length, 3, "current + new + confirm inputs");
    setInput(pwInputs[0] as HTMLInputElement, "pw-ok-123");
    setInput(pwInputs[1] as HTMLInputElement, "new-pw-456");
    setInput(pwInputs[2] as HTMLInputElement, "new-pw-456");
    submitForm((pwInputs[0] as HTMLInputElement).closest("form")!);
    await flush();
    const changeCall = calls.find((c) => c.path === "/api/account/security/password/change");
    equal(changeCall?.body, { currentPassword: "pw-ok-123", newPassword: "new-pw-456", revokeOtherSessions: true }, "change password payload");
    ok(invalidated === 1, "reauthRequired triggers session refresh callback");
    ok(host.textContent?.includes("Because a workspace requires two-factor authentication, sign in again"), "reauth banner shown");

    // Regenerate backup codes (requires password)
    act(() => { buttonByText(host, "Regenerate backup codes")!.click(); });
    await flush();
    const regenInput = host.querySelector('input[type="password"]') as HTMLInputElement;
    setInput(regenInput, "pw-ok-123");
    submitForm(regenInput.closest("form")!);
    await flush();
    ok(host.textContent?.includes("zzzzz-10000"), "regenerated codes rendered");

    // Session reset scrubs visible backup codes / inputs
    act(() => { resetPrivateSession(); });
    allowPrivateReads();
    await flush();
    ok(!host.textContent?.includes("zzzzz-10000"), "session reset clears sensitive backup code state");

    // Disable 2FA flow
    act(() => { buttonByText(host, "Disable")!.click(); });
    await flush();
    const disableInput = host.querySelector('input[type="password"]') as HTMLInputElement;
    setInput(disableInput, "pw-ok-123");
    submitForm(disableInput.closest("form")!);
    await flush();
    ok(host.textContent?.includes("Not enabled"), "2FA disabled updates UI state");
    unmount();

    // Workspace policy locks Disable button
    state.twoFactorEnabled = true;
    state.requiredByWorkspaces = [{ id: "ws_1", name: "Acme Corp" }];
    const lockedMount = await mount();
    const disableLocked = buttonByText(lockedMount.host, "Disable");
    ok(disableLocked?.disabled === true, "Disable button locked by org policy");
    ok(lockedMount.host.textContent?.includes("Acme Corp"), "workspace name displayed");
    lockedMount.unmount();
  }

  // 4. zh-TW localization renders properly
  {
    routeHandler = () => json({
      email: "tw@example.com",
      hasPassword: true,
      twoFactorEnabled: false,
      backupCodesRemaining: 0,
      requiredByWorkspaces: [],
    });
    const { host, unmount } = await mount({ zh: true });
    ok(host.textContent?.includes("安全性") && host.textContent?.includes("雙重驗證") && host.textContent?.includes("變更密碼"), "zh-TW copy localized");
    unmount();
  }

  console.log("settingsSecurity tests passed");
}

main().catch((err) => {
  console.error(err);
  require("process").exit(1);
});
