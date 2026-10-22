/**
 * CAD-1726: "Ask to change" as a reference chip.
 *
 * The composer cites it as a `ThreadRef {kind: "artifact", id}` — the chip.
 * `thread_send` stores refs on the entry for the board; it never hands them to
 * the agent, so at send time the chip is expanded into the message body
 * (`askBody`), which is what the master actually receives: the artifact id,
 * the version and the quoted passage. The ref id itself is only
 * `<artifact id>@v<version>#<n>` — the daemon refuses control characters and
 * more than 240 characters in an id, so the passage lives here, not in it.
 */
import type { ThreadRef } from "../../lib/types";

export const ASK_KIND = "artifact";
const ID = /^(art-[A-Za-z0-9]+)@v(\d+)#\d+$/;

interface AskInfo {
  artifact: string;
  version: number;
  title: string;
  /** The selected passage; empty means the whole artifact. */
  passage: string;
}

const known = new Map<string, AskInfo>();
let counter = 0;

export function makeAskRef(info: AskInfo): ThreadRef {
  const id = `${info.artifact}@v${info.version}#${++counter}`;
  known.set(id, info);
  return { kind: ASK_KIND, id };
}

export function parseAskRef(ref: ThreadRef): { artifact: string; version: number } | null {
  if (ref.kind !== ASK_KIND) return null;
  const m = ID.exec(ref.id);
  return m ? { artifact: m[1], version: Number(m[2]) } : null;
}

/** The chip's text: the passage and version (“passage” · v2), else the artifact. */
export function askLabel(ref: ThreadRef): string {
  const parsed = parseAskRef(ref);
  if (!parsed) return `${ref.kind}:${ref.id}`;
  const info = known.get(ref.id);
  const what = info?.passage ? `“${info.passage.replace(/\s+/g, " ").slice(0, 48)}${info.passage.length > 48 ? "…" : ""}”` : info?.title ?? parsed.artifact;
  return `${what} · v${parsed.version}`;
}

/** True when a draft cites an artifact (the composer asks what to change). */
export function citesArtifact(refs: readonly ThreadRef[]): boolean {
  return refs.some((r) => r.kind === ASK_KIND);
}

/**
 * The body the master receives: one header per artifact reference (id,
 * version, title) with the quoted passage, then the operator's own words.
 */
export function askBody(body: string, refs: readonly ThreadRef[]): string {
  const blocks: string[] = [];
  for (const ref of refs) {
    const parsed = parseAskRef(ref);
    if (!parsed) continue;
    const info = known.get(ref.id);
    const title = info?.title ? ` "${info.title}"` : "";
    const head = `About artifact ${parsed.artifact} (version ${parsed.version})${title}`;
    if (info?.passage) {
      blocks.push(`${head}, this passage:\n${info.passage.split("\n").map((l) => `> ${l}`).join("\n")}`);
    } else {
      blocks.push(`${head} (the whole artifact):`);
    }
  }
  return blocks.length ? `${blocks.join("\n\n")}\n\n${body}` : body;
}
