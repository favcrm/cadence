/**
 * The assistant rail opens collapsed on an app page, because the work area is
 * what the person came for — but never over a pending decision, and never
 * against an explicit choice.
 */
import { resolveChatCollapsed } from "../src/features/app-shell/chatCollapsed";

function equal(actual: unknown, expected: unknown, label: string) {
  if (actual !== expected) throw new Error(`${label}: ${String(actual)} !== ${String(expected)}`);
}

// No stored choice, an app installation, nothing waiting: the rail starts closed.
equal(resolveChatCollapsed(null, "install_crm", false), true, "app page defaults collapsed");

// A decision is waiting: the rail is the thing that must be visible.
equal(resolveChatCollapsed(null, "install_crm", true), false, "pending decision expands the rail");

// Outside an app installation (home), the assistant stays in front.
equal(resolveChatCollapsed(null, "", false), false, "home keeps the rail open");

// An explicit choice always wins, in both directions.
equal(resolveChatCollapsed(true, "install_crm", true), true, "explicit collapse wins over a decision");
equal(resolveChatCollapsed(false, "install_crm", false), false, "explicit expand wins on an app page");

console.log("chat collapsed defaults pass");
