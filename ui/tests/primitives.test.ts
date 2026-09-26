import { activateButtonHref, type ButtonClickLike } from "../src/ui/buttonActivate";
import {
  closedSelect,
  filterOptions,
  flipPlacement,
  openSelect,
  optionBadgeText,
  reduceOutside,
  reducePick,
  reduceQuery,
  reduceSelectKey,
  showsSearch,
  typeaheadIndex,
  type SelectOption,
} from "../src/ui/selectLogic";

// The UI test compiler has no @types/node. These two reads are the CSS files.
declare function require(name: string): {
  readFileSync(path: string, encoding: "utf8"): string;
  join(...parts: string[]): string;
};
declare const process: { cwd(): string };
const { readFileSync } = require("fs");
const { join } = require("path");

const root = process.cwd();

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function assert(cond: unknown, what: string): void {
  if (!cond) throw new Error(what);
}

const options: SelectOption[] = [
  { value: "amber", label: "Amber", hint: "first", group: "Letters" },
  { value: "alpine", label: "Alpine", badge: "Free (quota)", group: "Letters" },
  { value: "archive", label: "Archive", disabled: true, group: "Letters" },
  { value: "beta", label: "Beta", badge: "Paid", hint: "second", group: "More" },
  { value: "cursor", label: "Cursor plan", badge: "Cursor plan" },
];

// Keyboard: open, move, skip a disabled option, Home/End, Enter picks, Esc closes.
{
  let model = closedSelect();
  let step = reduceSelectKey(model, options, false, "", "ArrowDown", 0);
  assert(step.prevent && step.model.open && step.pick === null, "down opens");
  equal(step.model.active, 0, "down lands on the first enabled option");
  model = step.model;
  step = reduceSelectKey(model, options, false, "", "ArrowDown", 0);
  equal(step.model.active, 1, "down moves to Alpine");
  step = reduceSelectKey(step.model, options, false, "", "ArrowDown", 0);
  equal(step.model.active, 3, "down skips the disabled Archive");
  step = reduceSelectKey(step.model, options, false, "", "End", 0);
  equal(step.model.active, 4, "end");
  step = reduceSelectKey(step.model, options, false, "", "Home", 0);
  equal(step.model.active, 0, "home");
  step = reduceSelectKey(step.model, options, false, "", "ArrowUp", 0);
  equal(step.model.active, 0, "up stays on the first enabled option");
  const picked = reduceSelectKey(step.model, options, false, "", "Enter", 0);
  equal(picked.pick, "amber", "enter picks the highlighted option");
  assert(!picked.model.open, "enter closes");
  const again = reduceSelectKey(openSelect(options, "beta"), options, false, "beta", "Escape", 0);
  assert(again.prevent && !again.model.open && again.pick === null, "escape closes without picking");
}

// Space picks too. A disabled highlight cannot be picked.
{
  const open = { ...openSelect(options, "amber"), active: 2 };
  const blocked = reduceSelectKey(open, options, false, "amber", " ", 0);
  assert(blocked.pick === null && blocked.model.open, "space on a disabled option does not pick");
  const click = reducePick(open, options[2]);
  assert(click.pick === null && click.model.open, "click on a disabled option does not pick");
  const ok = reducePick(open, options[1]);
  equal(ok.pick, "alpine", "click picks an enabled option");
  assert(!ok.model.open, "click closes");
}

// Typeahead, including a second character inside the window and a disabled skip.
{
  const first = reduceSelectKey(closedSelect(), options, false, "", "a", 1000);
  equal(first.model.active, 0, "typeahead a → Amber");
  assert(first.model.open, "typeahead opens");
  const second = reduceSelectKey(first.model, options, false, "", "l", 1200);
  equal(second.model.active, 1, "typeahead al → Alpine");
  const wrapped = typeaheadIndex(options, "b", 4);
  equal(wrapped, 3, "typeahead wraps to Beta");
  const skipped = typeaheadIndex(
    [
      { value: "aa", label: "Aa", disabled: true },
      { value: "ab", label: "Ab" },
    ],
    "a",
    -1,
  );
  equal(skipped, 1, "typeahead skips a disabled match");
  const stale = reduceSelectKey(second.model, options, false, "", "b", 1200 + 501);
  equal(stale.model.active, 3, "a new character after the timeout starts a fresh search");
}

// Click-outside closes. Search filters on label, hint and badge text.
{
  const open = openSelect(options, "amber");
  const shut = reduceOutside(open);
  assert(!shut.open && shut.query === "", "click outside closes and clears");
  equal(reduceOutside(closedSelect()).open, false, "outside while closed is a no-op");
  equal(
    filterOptions(options, "quota").map((option) => option.value),
    ["alpine"],
    "search matches the badge",
  );
  equal(
    filterOptions(options, "SECOND").map((option) => option.value),
    ["beta"],
    "search matches the hint, case-insensitive",
  );
  equal(
    filterOptions(options, "letters").map((option) => option.value),
    ["amber", "alpine", "archive"],
    "search matches the group",
  );
  const queried = reduceQuery(open, options, "paid", "amber");
  equal(queried.query, "paid", "query is stored");
  equal(queried.active, 0, "the filtered list highlights the remaining match");
  equal(filterOptions(options, "paid")[queried.active]?.value, "beta", "active index is into the filtered list");
  assert(showsSearch(options, true), "searchable forces the field");
  assert(!showsSearch(options), "eight or fewer options stay closed");
  assert(showsSearch(Array.from({ length: 9 }, (_, i) => ({ value: String(i), label: String(i) }))), "nine options open search");
}

// The badge is text, not an image or an aria-only label.
{
  equal(optionBadgeText(options[1]!), "Free (quota)", "badge text");
  equal(optionBadgeText({ value: "x", label: "X", badge: "  " }), null, "blank badge is absent");
  equal(optionBadgeText(options[0]!), null, "no badge");
}

// The popover flips above the trigger when the bottom edge cannot hold it.
{
  equal(flipPlacement({ top: 100, bottom: 132 }, 200, 800), "below", "room below");
  equal(flipPlacement({ top: 700, bottom: 732 }, 200, 800), "above", "near the bottom");
  equal(flipPlacement({ top: 20, bottom: 52 }, 200, 80), "below", "no room above either — stay below");
}

// href Buttons route a plain click through navigate and leave modified clicks alone.
{
  const calls: string[] = [];
  const event = (over: Partial<ButtonClickLike> = {}): ButtonClickLike => ({
    defaultPrevented: false,
    button: 0,
    metaKey: false,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    preventDefault() {
      this.defaultPrevented = true;
    },
    ...over,
  });
  const plain = event();
  activateButtonHref("/wiki", plain, (href) => calls.push(href));
  equal(calls, ["/wiki"], "plain click navigates");
  assert(plain.defaultPrevented, "plain click does not reload");
  const modified = event({ metaKey: true });
  activateButtonHref("/wiki", modified, (href) => calls.push(href));
  equal(calls, ["/wiki"], "a modified click is left to the browser");
  assert(!modified.defaultPrevented, "modified click is not cancelled");
  const prevented = event();
  prevented.preventDefault();
  activateButtonHref("/nope", prevented, (href) => calls.push(href));
  equal(calls, ["/wiki"], "an already-prevented click does not navigate");
}

// Button centring: the .btn rule's specified values are what getComputedStyle
// returns for justify-content and line-height (they are not inherited).
function declarations(css: string, selector: string): Record<string, string> {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = css.match(new RegExp(`${escaped}\\s*\\{([^}]+)\\}`));
  if (!match?.[1]) throw new Error(`${selector} not found`);
  const out: Record<string, string> = {};
  for (const part of match[1].split(";")) {
    const split = part.indexOf(":");
    if (split === -1) continue;
    out[part.slice(0, split).trim()] = part.slice(split + 1).trim();
  }
  return out;
}

function computedButton(css: string): { justifyContent: string; lineHeight: string } {
  const body = declarations(css, "body");
  const btn = declarations(css, ".btn");
  return {
    justifyContent: btn["justify-content"] ?? "normal",
    lineHeight: btn["line-height"] ?? body["line-height"] ?? "normal",
  };
}

for (const file of [join(root, "src/styles.css"), join(root, "../design/kit.css")]) {
  const css = readFileSync(file, "utf8");
  const computed = computedButton(css);
  equal(computed.justifyContent, "center", `${file} justify-content`);
  equal(computed.lineHeight, "1", `${file} line-height`);
  assert(css.includes("prefers-reduced-motion"), `${file} reduced motion`);
  assert(declarations(css, ".select-badge")["margin-left"] === "auto", `${file} badge sits on the right`);
}

const buttonSource = readFileSync(join(root, "src/ui/Button.tsx"), "utf8");
assert(buttonSource.includes("activateButtonHref(href, event, navigate"), "href Button calls navigate");
const selectSource = readFileSync(join(root, "src/ui/Select.tsx"), "utf8");
assert(selectSource.includes("optionBadgeText"), "the listbox renders the badge string");
assert(selectSource.includes("reduceOutside"), "click-outside uses the closer");
assert(selectSource.includes('className="select-badge"'), "badge is a text element");
