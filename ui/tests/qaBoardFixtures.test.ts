/**
 * CAD-2291: the QA board's fixture payloads pass the UI's own client parsers
 * and loaders — the same check `qa-board up` runs before it serves them.
 */
import { checkBrowser, loadFixtures, validateFixtures } from "../scripts/qa-board/validate";

declare const process: { cwd(): string };

async function main() {
  const fixtures = loadFixtures(`${process.cwd()}/scripts/qa-board/fixtures`);
  const failures = await validateFixtures(fixtures, "https://127.0.0.1:3199/live");
  if (failures.length > 0) throw new Error(`QA board fixtures refused:\n${failures.join("\n")}`);

  // The check refuses what the UI refuses: a drifted payload fails, named.
  if (checkBrowser({ state: "taken-over", signedIn: false }) === null) throw new Error("an unknown browser state passed");
  const drifted = await validateFixtures({ ...fixtures, "browser-active": { state: "active", signedIn: "yes" } }, "https://127.0.0.1:3199/live");
  if (!drifted.some((line) => line.startsWith("browser-active.json:"))) throw new Error(`a drifted payload was not named: ${drifted.join("; ")}`);
}

main().then(
  () => console.log("qaBoardFixtures: ok"),
  (error) => {
    console.error(error);
    throw error;
  },
);
