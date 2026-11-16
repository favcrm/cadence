import { boardSupports } from "../src/lib/capabilities";
import { Resource } from "../src/lib/cacheCore";
import { navItems } from "../src/lib/router";

function equal(actual: unknown, expected: unknown, what: string): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${what}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

async function main() {
  // Local/native boards support every optional capability by default.
  equal(boardSupports(null, "my_work"), true, "pre-meta defaults to supported");
  equal(boardSupports({ hosted: false }, "milestones"), true, "local board supports milestones");

  // Hosted boards require explicit advertisement in /api/meta.
  equal(boardSupports({ hosted: true }, "my_work"), false, "hosted board without flag hides my_work");
  equal(boardSupports({ hosted: true, my_work: true }, "my_work"), true, "hosted board with flag enables my_work");
  equal(boardSupports({ hosted: true, wiki: false }, "wiki"), false, "hosted board with wiki=false hides wiki");

  // navItems hides unsupported screens alongside noWiki.
  const hostedScreens = navItems(true, ["myWork", "threads"]).map((item) => item.screen);
  equal(hostedScreens.includes("wiki"), false, "navItems hides wiki");
  equal(hostedScreens.includes("myWork"), false, "navItems hides myWork");
  equal(hostedScreens.includes("threads"), false, "navItems hides threads");
  equal(hostedScreens.includes("projects"), true, "navItems keeps projects");

  // Resource treats 501 as a permanent capability refusal for invalidate/revalidate.
  let calls = 0;
  const res = new Resource(async () => {
    calls += 1;
    throw Object.assign(new Error("not_implemented"), { status: 501 });
  });
  await res.refresh();
  equal(calls, 1, "initial refresh calls fetcher once");
  await res.invalidate();
  await res.revalidate();
  equal(calls, 1, "invalidate and revalidate do not retry after 501");
  res.reset();
  await res.revalidate();
  equal(calls, 2, "reset clears 501 latch");

  console.log("capabilities501 checks passed");
}

void main().catch((err: unknown) => {
  setTimeout(() => {
    throw err;
  }, 0);
});
