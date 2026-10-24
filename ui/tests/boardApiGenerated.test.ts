/** CAD-1792: the committed TypeScript is exactly what the committed JSON
 *  Schema generates (the schema itself is checked against the Rust types by
 *  `cargo test -p cadence-core`). */
declare function require(name: string): any;
declare const process: { cwd(): string; execPath: string };
export {};
const { spawnSync } = require("child_process");
const run = spawnSync(process.execPath, ["scripts/gen-board-api-types.mjs", "--check"], {
  cwd: process.cwd(),
  encoding: "utf8",
});
if (run.status !== 0) throw new Error(`generated board API types are stale: ${run.stderr}`);
console.log("generated board API types are current");
