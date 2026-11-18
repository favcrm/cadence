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

const LITERAL = /"((?:[^"\\]|\\.)*)"/g;
const unquote = (body: string): string => JSON.parse(`"${body}"`);

/** Every string literal inside each `t(...)` call: ternaries and concatenations included. */
function insideT(source: string): string[] {
  const labels: string[] = [];
  for (const start of source.matchAll(/\bt\(/g)) {
    let depth = 1;
    let index = (start.index ?? 0) + start[0].length;
    const from = index;
    let quote: string | null = null;
    while (index < source.length && depth > 0) {
      const char = source[index];
      if (quote !== null) {
        if (char === "\\") index += 1;
        else if (char === quote) quote = null;
      } else if (char === '"' || char === "'" || char === "`") quote = char;
      else if (char === "(") depth += 1;
      else if (char === ")") depth -= 1;
      index += 1;
    }
    const args = source.slice(from, index - 1);
    for (const match of args.matchAll(LITERAL)) {
      // An id compared inside a ternary (`kind === "saved" ? … : …`) is not copy.
      const before = args.slice(0, match.index ?? 0);
      const after = args.slice((match.index ?? 0) + match[0].length);
      if (/[!=]==\s*$/.test(before) || /^\s*[!=]==/.test(after)) continue;
      labels.push(unquote(match[1]));
    }
  }
  return labels;
}

/**
 * Labels the agent page translates later, outside a literal `t()` call:
 * `Detail label="…"`, RowMenu items (`label: "…"`), `Facts` keys
 * (`["Source", …]`), and the `text:` strings helpers hand to `t()`.
 */
function laterLabels(source: string): string[] {
  const labels: string[] = [];
  const patterns = [
    /\blabel="((?:[^"\\]|\\.)*)"/g,
    /\blabel:\s*"((?:[^"\\]|\\.)*)"/g,
    /\[\s*"([A-Z](?:[^"\\]|\\.)*)",\s*(?:t\(|[A-Za-z_][\w.]*|`|"|\()/g,
  ];
  for (const pattern of patterns) for (const match of source.matchAll(pattern)) labels.push(unquote(match[1]));
  for (const match of source.matchAll(/text:\s*[^"\n]*"((?:[^"\\]|\\.)*)"(?:\s*:\s*"((?:[^"\\]|\\.)*)")?/g)) {
    for (const group of [match[1], match[2]]) if (group !== undefined) labels.push(unquote(group));
  }
  return labels;
}

/**
 * Calls that pass one argument to `t()` later: the skill drawer's
 * `decide(job, doneMessage, …)` toasts its second argument.
 */
const TRANSLATED_ARGS: Record<string, number> = { decide: 1 };

/** The literal at each translated argument position of those calls. */
function translatedArgs(source: string): string[] {
  const labels: string[] = [];
  for (const [callee, position] of Object.entries(TRANSLATED_ARGS)) {
    for (const start of source.matchAll(new RegExp(`\\b${callee}\\(`, "g"))) {
      const args: string[] = [];
      let depth = 1;
      let quote: string | null = null;
      let current = "";
      for (let index = (start.index ?? 0) + start[0].length; index < source.length && depth > 0; index += 1) {
        const char = source[index]!;
        if (quote !== null) {
          if (char === "\\") {
            current += char + source[index + 1];
            index += 1;
            continue;
          }
          if (char === quote) quote = null;
        } else if (char === '"' || char === "'" || char === "`") quote = char;
        else if ("([{".includes(char)) depth += 1;
        else if (")]}".includes(char)) depth -= 1;
        if (depth === 0) break;
        if (depth === 1 && quote === null && char === ",") {
          args.push(current);
          current = "";
        } else current += char;
      }
      args.push(current);
      const literal = /^\s*"((?:[^"\\]|\\.)*)"\s*$/.exec(args[position] ?? "");
      if (literal) labels.push(unquote(literal[1]!));
    }
  }
  return labels;
}

/** Literals that are not English copy: file names and bare placeholders. */
const NOT_COPY = /^(?:.*\.(?:md|json|tsx?|js)|\{[a-z]+\})$/;

function englishLabels(source: string): string[] {
  return [...insideT(source), ...laterLabels(source), ...translatedArgs(source)].filter((label) => /[A-Za-z]/.test(label) && !NOT_COPY.test(label.trim()));
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
