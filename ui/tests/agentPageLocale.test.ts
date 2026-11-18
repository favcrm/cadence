// CAD-2451: every English label the agent page passes to `t()` has a zh-TW
// entry, so a new Skills tab or panel label cannot ship untranslated.
import { translateUiText } from "../src/lib/locale";

export {};
declare function require(name: string): any;
declare const process: { cwd(): string; exitCode?: number };

const fs = require("node:fs");
const path = require("node:path");

/** Source files of the agent page, relative to the ui package. */
const FILES = [
  "src/features/agents/AgentTabs.tsx",
  "src/features/agents/SkillPanels.tsx",
  "src/features/agents/workspaceSkills.ts",
];

/** `t("…")` literals, plus the `text:` strings helpers hand to `t()` later. */
function englishLabels(source: string): string[] {
  const labels: string[] = [];
  const literal = '"((?:[^"\\\\]|\\\\.)*)"';
  for (const match of source.matchAll(new RegExp(`\\bt\\(\\s*${literal}`, "g"))) labels.push(JSON.parse(`"${match[1]}"`));
  for (const match of source.matchAll(new RegExp(`text:\\s*[^"\\n]*${literal}(?:\\s*:\\s*${literal})?`, "g"))) {
    for (const group of [match[1], match[2]]) if (group !== undefined) labels.push(JSON.parse(`"${group}"`));
  }
  return labels;
}

function main() {
  const missing: string[] = [];
  let checked = 0;
  for (const file of FILES) {
    const source = fs.readFileSync(path.join(process.cwd(), file), "utf8");
    for (const label of englishLabels(source)) {
      checked += 1;
      if (translateUiText(label, "zh-TW") === label) missing.push(`${file}: ${JSON.stringify(label)}`);
    }
  }
  if (checked < 100) throw new Error(`only ${checked} labels found; the extractor no longer matches the source`);
  if (missing.length > 0) throw new Error(`zh-TW is missing for ${missing.length} agent-page labels:\n${missing.join("\n")}`);
  console.log(`agentPageLocale checks passed (${checked} labels)`);
}

try {
  main();
} catch (error) {
  console.error(error);
  process.exitCode = 1;
}
