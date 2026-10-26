import { NAV, navItems } from "../src/lib/router";

function equal(actual: unknown, expected: unknown, what: string): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${what}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

// A local board keeps every screen.
equal(navItems(false).map((item) => item.screen), NAV.map((item) => item.screen), "local nav is unchanged");

// A hosted tenant does not serve /api/wiki; the nav must not promise it.
const hosted = navItems(true).map((item) => item.screen);
equal(hosted.includes("wiki"), false, "hosted nav hides the wiki");
equal(hosted.length, NAV.length - 1, "hosted nav keeps every other screen");
equal(hosted.includes("projects"), true, "hosted nav keeps projects");
equal(hosted.includes("settings"), true, "hosted nav keeps settings");

console.log("hosted nav checks passed");
