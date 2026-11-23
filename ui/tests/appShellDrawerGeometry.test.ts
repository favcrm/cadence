/**
 * The promoted app frame keeps its own width.
 *
 * The board-wide drawer promotes the outlet node (the frame that already holds
 * the mounted app) to the whole window height. For a while it also narrowed
 * that node to a 30rem column, which turned a screen's own content into the
 * drawer: Social Content's Library reflowed into 480px and the app's panel
 * covered what was left. The geometry is one CSS block, so the guard reads the
 * block rather than a rendered box (the UI test compiler has no layout engine).
 */
export {};

declare function require(name: string): {
  readFileSync(path: string, encoding: "utf8"): string;
};
declare const process: { cwd(): string };
const { readFileSync } = require("fs");

const css = readFileSync(`${process.cwd()}/src/features/app-shell/app-shell.css`, "utf8");

/** The declaration block for one exact selector, comments stripped. */
function block(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = css.replace(/\/\*[\s\S]*?\*\//g, "").match(
    new RegExp(`${escaped}\\s*\\{([^}]*)\\}`),
  );
  if (!match) throw new Error(`no rule for ${selector}`);
  return match[1]!;
}

function assert(cond: unknown, what: string): void {
  if (!cond) throw new Error(what);
}

const promoted = block('.app-shell[data-screen-drawer="true"] .app-shell-outlet');

// Full height is the point of the promotion: the app's panel reaches the
// window, not the pane it normally lives in.
assert(/height:\s*100dvh/.test(promoted), "the promoted frame is the window height");
assert(/max-height:\s*100dvh/.test(promoted), "the promoted frame may not exceed the window");
assert(/position:\s*fixed/.test(promoted), "the promoted frame is fixed to the viewport");

// Width is not the shell's to change. `inset: 0` plus `width: auto` takes the
// viewport without reflowing the app into a column.
assert(/inset:\s*0/.test(promoted), "the promoted frame takes the viewport on both axes");
assert(/width:\s*auto/.test(promoted), "the promoted frame keeps the app's own width");

// The regression itself, named: any fixed or computed width here re-creates
// the 480px column the app's content shrank into.
assert(!/width:\s*min\(/.test(promoted), "the promoted frame is never sized to a 30rem column");
assert(!/width:\s*\d/.test(promoted), "the promoted frame declares no fixed width");
assert(!/30rem/.test(promoted), "no 30rem column remains in the promotion");

// The side chrome that only existed to make a narrow column read as a drawer.
assert(!/border-left/.test(promoted), "a viewport-wide frame has no drawer edge");
assert(!/box-shadow/.test(promoted), "a viewport-wide frame casts no drawer shadow");

// The scrim still covers the whole board while the frame is promoted.
const scrim = block(".app-shell-screen-scrim");
assert(/position:\s*fixed/.test(scrim), "the scrim is fixed");
assert(/inset:\s*0/.test(scrim), "the scrim covers the window");
