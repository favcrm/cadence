#!/usr/bin/env node
/**
 * Generate the hosted copy of the `company-outputs` skill from its one source,
 * `skills/company-outputs/`. Hosted has no filesystem to install guidance into,
 * so the same text travels in the runtime bundle: `SKILL.md` is the body and
 * the references load on demand, with the native column and detail cut from the
 * lookup because hosted cannot act on them. `bun run build` regenerates this file, so CI's
 * clean-dist check fails a skill edit that did not rebuild the runtime.
 */
import { createHash } from "node:crypto";
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const source = join(here, "..", "..", "skills", "company-outputs");
const target = join(here, "..", "src", "company-outputs.generated.ts");

const NATIVE_DETAIL = "The rest of this file is the native detail.";

function hostedText(file, text) {
  if (file !== "cadence.md") return text;
  const cut = text.indexOf(NATIVE_DETAIL);
  if (cut < 0) throw new Error("company-outputs cadence.md: native detail marker missing");
  // Keep the capability and hosted cells of the table; a hosted agent must not
  // be pointed at commands its workspace does not have.
  const lookup = text
    .slice(0, cut)
    .trimEnd()
    .split("\n")
    .map((line) => {
      if (!line.startsWith("|")) return line;
      const cells = line.split("|");
      if (cells.length !== 5) throw new Error(`company-outputs cadence.md: unexpected row: ${line}`);
      return `|${cells[1]}|${cells[3]}|`;
    });
  return `${lookup.join("\n")}\n`;
}

function renderCompanyOutputs() {
  const text = readFileSync(join(source, "SKILL.md"), "utf8");
  const parsed = /^---\nname: (.+)\ndescription: (.+)\n---\n\n([\s\S]*)$/.exec(text);
  if (!parsed) throw new Error("company-outputs SKILL.md: unexpected frontmatter");
  const [, name, description, body] = parsed;
  const resources = readdirSync(join(source, "references"))
    .sort()
    .map((file) => ({
      kind: "reference",
      path: `references/${file}`,
      content: hostedText(file, readFileSync(join(source, "references", file), "utf8")),
    }));
  const digest = createHash("sha256")
    .update(JSON.stringify({ description, body, resources }))
    .digest("hex")
    .slice(0, 16);
  const skill = { name, version: digest, description, body, resources };
  return `// Generated from skills/company-outputs by scripts/gen-company-outputs.mjs. Do not edit.
import type { SkillManifest } from "agents/skills";

export const COMPANY_OUTPUTS_FINGERPRINT = ${JSON.stringify(`tenant-company-outputs-${digest}`)};

export const COMPANY_OUTPUTS_SKILL = ${JSON.stringify(skill, null, 2)} as unknown as SkillManifest["skills"][number];
`;
}

writeFileSync(target, renderCompanyOutputs());
