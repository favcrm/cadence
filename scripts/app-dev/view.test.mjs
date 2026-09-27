import assert from "node:assert/strict";
import { test } from "node:test";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { resolve } from "node:path";
const repo = fileURLToPath(new URL("../../", import.meta.url));
const tools = createRequire(resolve(repo, "ui/package.json"));

test("actual studio mounts source grid, batch run, editor, revision guard and local calendar plan with accessible drawer", async () => {
  const { Window } = tools("happy-dom");
  const window = new Window();
  globalThis.window = window;
  globalThis.document = window.document;
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  const React = tools("react"),
    { createRoot } = tools("react-dom/client"),
    ts = tools("typescript");
  const icons = new Map();
  for (const name of [
    "CubeIcon",
    "File01Icon",
    "Tick02Icon",
    "MailSend01Icon",
    "ContrastIcon",
  ]) {
    const id = `@hugeicons/core-free-icons/${name}`;
    icons.set(
      id,
      await import(
        pathToFileURL(tools.resolve(id).replace("/dist/cjs/", "/dist/esm/"))
          .href
      ),
    );
  }
  function load(file) {
    const local = createRequire(file),
      exports = {};
    const code = ts.transpileModule(
      readFileSync(file, "utf8").replaceAll(
        "__APP_DEV_REVISION__",
        '"test-base"',
      ),
      {
        compilerOptions: {
          jsx: ts.JsxEmit.ReactJSX,
          module: ts.ModuleKind.CommonJS,
          target: ts.ScriptTarget.ES2022,
        },
      },
    ).outputText;
    new Function("require", "exports", code)(
      (id) =>
        icons.has(id)
          ? icons.get(id)
          : id.startsWith(".")
            ? id.endsWith(".jsx")
              ? load(resolve(file, "..", id))
              : local(id)
            : tools(id),
      exports,
    );
    return exports;
  }
  const View = load(
    resolve(repo, "app-previews/social-content/SocialContent.jsx"),
  ).default;
  const source = createRequire(
    resolve(repo, "app-previews/social-content/SocialContent.jsx"),
  );
  const { studio } = source("./store.mjs");
  studio.reset();
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const act = React.act;
  async function click(text) {
    const button = [...host.querySelectorAll("button")].find(
      (b) => b.textContent.trim() === text,
    );
    assert.ok(button, text);
    assert.ok(!button.disabled, text);
    await act(() => button.click());
    return button;
  }
  async function field(selector, value) {
    const element = host.querySelector(selector);
    assert.ok(element, selector);
    if (element.tagName === "SELECT") {
      await act(() => {
        element.value = value;
        element.dispatchEvent(new window.Event("change", { bubbles: true }));
      });
      return;
    }
    const key = Object.keys(element).find((k) => k.startsWith("__reactProps$"));
    await act(() => element[key].onChange({ target: { value } }));
  }
  try {
    await act(() => root.render(React.createElement(View)));
    assert.ok(host.textContent.includes("Schedule"));
    assert.equal(host.querySelectorAll(".calendar-day").length, 7);
    const themeButton = host.querySelector('button[aria-label^="Theme:"]');
    for (const theme of ["light", "dark", "system"]) {
      await act(() => themeButton.click());
      assert.equal(
        document.documentElement.getAttribute("data-theme"),
        theme === "system" ? null : theme,
      );
      assert.equal(
        window.localStorage.getItem("cadence-theme"),
        theme === "system" ? null : theme,
      );
    }
    const initialWeek = host.querySelector(
      ".section-toolbar + .kicker",
    ).textContent;
    await act(() => host.querySelector('[aria-label="Next week"]').click());
    assert.notEqual(
      host.querySelector(".section-toolbar + .kicker").textContent,
      initialWeek,
    );
    await click("This week");
    const firstDay = source("./fixtures.mjs").fixtureWeek;
    const nextYear = new Date(
      `${Number(firstDay.slice(0, 4)) + 1}-01-01T12:00:00Z`,
    );
    const yearWeek = Math.floor(
      (nextYear - new Date(`${firstDay}T12:00:00Z`)) / (7 * 86400000),
    );
    for (let i = 0; i < yearWeek; i++)
      await act(() => host.querySelector('[aria-label="Next week"]').click());
    assert.ok(
      host
        .querySelector(".section-toolbar + .kicker")
        .textContent.includes(String(nextYear.getUTCFullYear())),
    );
    await click("This week");
    await click("Library");
    assert.equal(host.querySelectorAll(".source-card").length, 3);
    const inspect = host.querySelector(
      '[aria-label="Inspect source source-3"]',
    );
    inspect.focus();
    await act(() => inspect.click());
    assert.ok(host.querySelector('[role="dialog"]'));
    assert.match(host.textContent, /Immutable fixture source/);
    await act(() =>
      document.dispatchEvent(
        new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      ),
    );
    assert.equal(host.querySelector('[role="dialog"]'), null);
    assert.equal(document.activeElement, inspect);
    for (const id of ["source-3", "source-4"])
      await act(() =>
        host.querySelector(`[aria-label="Select source ${id}"]`).click(),
      );
    await click("Draft 2 posts");
    assert.equal(host.querySelectorAll(".run-card tbody tr").length, 2);
    assert.equal(studio.read().runs[0].items.length, 2);
    const item = studio.read().runs[0].items[0];
    await click(`${item.sourceId} → ${item.postId}`);
    assert.ok(host.querySelector(".editor-grid"));
    assert.equal(
      host.querySelector("#caption").value,
      studio.read().sources.find((s) => s.id === item.sourceId).text,
    );
    await field("#caption", "Human caption for the scheduled fixture");
    const button = (text) =>
      [...host.querySelectorAll("button")].find(
        (value) => value.textContent.trim() === text,
      );
    await field(
      "#schedule-at",
      `${source("./fixtures.mjs").fixtureWeek}T14:30`,
    );
    assert.ok(
      button("Save revision").disabled,
      "unsaved planning blocks caption save",
    );
    assert.ok(
      button("Save local plan").disabled,
      "unsaved caption blocks planning save",
    );
    assert.ok(
      button("Use fixture image").disabled,
      "material action cannot discard unsaved caption/planning",
    );
    await click("Discard local plan");
    assert.equal(
      host.querySelector("#caption").value,
      "Human caption for the scheduled fixture",
    );
    await click("Save revision");
    await field(
      "#schedule-at",
      `${source("./fixtures.mjs").fixtureWeek}T14:30`,
    );
    await field("#caption", "Unsaved second caption");
    assert.ok(button("Save revision").disabled);
    assert.ok(button("Save local plan").disabled);
    await click("Discard");
    assert.equal(
      host.querySelector("#schedule-at").value,
      `${source("./fixtures.mjs").fixtureWeek}T14:30`,
      "discard caption preserves unsaved planning",
    );
    await click("Save local plan");
    await click("Mark reviewed");
    await click("Approve local plan");
    await click("Stage locally");
    assert.match(host.textContent, /Nothing published/);
    const staged = studio.read().posts.find((p) => p.id === item.postId);
    assert.equal(staged.outbox.externalReceipt, null);
    await field("#caption", "Edited after approval");
    await click("Save revision");
    assert.equal(
      studio.read().posts.find((p) => p.id === item.postId).approvalRevision,
      null,
    );
    assert.ok(
      [...host.querySelectorAll("button")].find(
        (b) => b.textContent === "Stage locally",
      ).disabled,
    );
    await click("Close");
    await click("Home");
    assert.ok(
      host
        .querySelector(".calendar")
        .textContent.includes("Edited after approval"),
    );
    await click("Board");
    assert.equal(host.querySelectorAll(".board-lane").length, 6);
    for (const lane of host.querySelectorAll(".board-lane")) {
      const body = lane.querySelector(".board-lane-body");
      assert.ok(body, "each board lane owns its scroll body");
      assert.equal(body.getAttribute("role"), "region");
      assert.equal(body.tabIndex, 0);
      assert.ok(
        body
          .getAttribute("aria-label")
          .includes(lane.querySelector("h3").textContent),
      );
      assert.equal(
        Number(lane.querySelector("header .num").textContent),
        body.querySelectorAll(".post-card").length,
      );
    }
    await click("Calendar");
    const plannedDay = [...host.querySelectorAll(".calendar-day")].find((day) =>
      day.textContent.includes("Edited after approval"),
    );
    assert.equal(plannedDay.dataset.date, source("./fixtures.mjs").fixtureWeek);
    assert.ok(plannedDay.textContent.includes("14:30"));
    await click("Library");
    await field("#context", "Kura Ramen");
    await act(() =>
      host.querySelector('[aria-label="Select source source-3"]').click(),
    );
    await field("#context", "Velvet Padel");
    assert.ok(
      button("Draft posts").disabled,
      "context change clears hidden selection",
    );
    await act(() =>
      host.querySelector('[aria-label="Select source source-4"]').click(),
    );
    await click("Draft 1 posts");
    const contextRun = studio.read().runs[0];
    assert.equal(
      studio.read().posts.find((post) => post.id === contextRun.items[0].postId)
        .brand,
      "Velvet Padel",
    );
    assert.ok(
      ![...host.querySelectorAll(".run-card tbody")].some((table) =>
        table.textContent.includes("source-3"),
      ),
      "Runs show only current-context items",
    );
    await click("Home");
    await click("Board");
    for (const lane of host.querySelectorAll(".board-lane")) {
      const body = lane.querySelector(".board-lane-body");
      if (!body.querySelector(".post-card")) {
        assert.match(body.textContent, /No posts in/);
        assert.equal(lane.querySelector("header .num").textContent, "0");
      }
    }
    assert.ok(
      host.querySelector(".board-empty"),
      "scoped empty lanes are explicit",
    );
    await click("Settings");
    assert.match(host.textContent, /protected terms/i);
    studio.reset();
    for (let hour = 9; hour < 16; hour++) {
      const id = studio.batch(["source-3"]).items[0].postId;
      studio.material(
        id,
        {
          scheduleAt: `${source("./fixtures.mjs").fixtureWeek}T${String(hour).padStart(2, "0")}:00`,
        },
        1,
      );
    }
    await act(() =>
      root.render(React.createElement(View, { key: "crowded-week" })),
    );
    const busyDay = host.querySelector(".calendar-day");
    const busyRegion = busyDay.querySelector(".day-posts");
    assert.equal(busyRegion.getAttribute("role"), "region");
    assert.equal(busyRegion.tabIndex, 0);
    assert.equal(
      busyRegion.getAttribute("aria-label"),
      `Posts planned for ${source("./fixtures.mjs").fixtureWeek}`,
    );
    assert.equal(
      busyDay.querySelectorAll(".day-posts .post-card").length,
      8,
      "busy day retains all cards in one horizontal region",
    );
    const times = [
      ...busyDay.querySelectorAll(".post-card > small:first-child"),
    ].map((value) => value.textContent.slice(0, 5));
    assert.deepEqual(
      times,
      [...times].sort(),
      "day cards ordered by planned time",
    );
    Object.defineProperties(busyRegion, {
      clientWidth: { configurable: true, value: 500 },
      scrollWidth: { configurable: true, value: 1984 },
    });
    busyRegion.scrollBy = ({ left, behavior }) => {
      assert.equal(Math.abs(left), 248, "advance one card plus its gap");
      assert.equal(
        behavior,
        "auto",
        "reduced motion disables smooth scrolling",
      );
      busyRegion.scrollLeft += left;
      busyRegion.dispatchEvent(new window.Event("scroll"));
    };
    window.matchMedia = () => ({ matches: true });
    await act(() => window.dispatchEvent(new window.Event("resize")));
    const dayLabel = source("./fixtures.mjs").fixtureWeek;
    const arrow = (direction) =>
      busyDay.querySelector(
        `button[aria-label="Scroll ${direction} posts for ${dayLabel}"]`,
      );
    assert.equal(
      arrow("later").hidden,
      false,
      "overflow exposes the next-card button",
    );
    const laterNode = arrow("later");
    assert.equal(arrow("earlier").hidden, true, "no backward arrow at start");
    await act(() => arrow("later").click());
    assert.equal(busyRegion.scrollLeft, 248);
    assert.equal(
      arrow("earlier").hidden,
      false,
      "backward arrow appears after advancing",
    );
    await act(() => arrow("earlier").click());
    assert.equal(busyRegion.scrollLeft, 0);
    await act(() => {
      busyRegion.scrollLeft = 1484;
      busyRegion.dispatchEvent(new window.Event("scroll"));
    });
    assert.equal(
      arrow("later").hidden,
      true,
      "forward arrow disappears at the end",
    );
    assert.equal(
      arrow("later"),
      laterNode,
      "direction updates retain the same button node",
    );
    Object.defineProperty(busyRegion, "clientWidth", { value: 1984 });
    await act(() => window.dispatchEvent(new window.Event("resize")));
    assert.equal(
      arrow("earlier").hidden,
      true,
      "resize removes arrows without overflow",
    );
    const calendar = host.querySelector(".schedule-view.calendar");
    assert.equal(calendar.tabIndex, 0);
    assert.equal(calendar.getAttribute("aria-label"), "Week calendar days");
    Object.defineProperties(calendar, {
      clientHeight: { configurable: true, value: 260 },
      scrollHeight: { configurable: true, value: 900 },
    });
    calendar.scrollBy = ({ top, behavior }) => {
      assert.equal(Math.abs(top), 90, "calendar arrow advances one day row");
      assert.equal(behavior, "auto");
      calendar.scrollTop += top;
      calendar.dispatchEvent(new window.Event("scroll"));
    };
    await act(() => window.dispatchEvent(new window.Event("resize")));
    const calendarArrow = (direction) =>
      host.querySelector(
        `button[aria-label="Scroll ${direction} calendar days"]`,
      );
    assert.equal(calendarArrow("earlier").hidden, true);
    assert.equal(calendarArrow("later").hidden, false);
    await act(() => calendarArrow("later").click());
    assert.equal(calendar.scrollTop, 90);
    assert.equal(calendarArrow("earlier").hidden, false);
    await act(() => {
      calendar.scrollTop = 640;
      calendar.dispatchEvent(new window.Event("scroll"));
    });
    assert.equal(calendarArrow("later").hidden, true);
    Object.defineProperty(calendar, "clientHeight", { value: 900 });
    await act(() => window.dispatchEvent(new window.Event("resize")));
    assert.equal(calendarArrow("earlier").hidden, true);
  } finally {
    await act(() => root.unmount());
    await window.happyDOM.close();
  }
});
