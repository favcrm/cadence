import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../../lib/api";
import type { AudienceScope } from "../audienceClient";
import {
  checkCampaignId,
  checkContent,
  friendlyCampaignError,
  parseContentDoc,
  type CampaignBlock,
  type ContentDoc,
} from "../campaignGrammar";
import { contentClient } from "../contentClient";

export interface EditorBlock {
  key: number;
  kind: CampaignBlock["type"];
  text: string;
  label: string;
  url: string;
}

/** What the operator is editing. `html === null` means the blocks are
 *  the body; a string means the pasted/edited HTML is (never both —
 *  the save sends exactly one). `ownText` is the "Write my own"
 *  override; off means the host generates the plain text. */
export interface EmailDraft {
  subject: string;
  preheader: string;
  blocks: EditorBlock[];
  html: string | null;
  ownText: boolean;
  text: string;
}

let editorKey = 1;

export function blocksFromDoc(doc: ContentDoc | null): EditorBlock[] {
  if (doc === null || doc.blocks.length === 0) {
    return [{ key: editorKey++, kind: "paragraph", text: "", label: "", url: "" }];
  }
  return doc.blocks.map((block) =>
    block.type === "button"
      ? { key: editorKey++, kind: "button" as const, text: "", label: block.label, url: block.url }
      : { key: editorKey++, kind: block.type, text: block.text, label: "", url: "" },
  );
}

export function newBlock(kind: CampaignBlock["type"]): EditorBlock {
  return { key: editorKey++, kind, text: "", label: "", url: kind === "button" ? "https://" : "" };
}

export function blocksToGrammar(blocks: EditorBlock[]): CampaignBlock[] {
  return blocks.map((block) => {
    if (block.kind === "heading") return { type: "heading", text: block.text };
    if (block.kind === "paragraph") return { type: "paragraph", text: block.text };
    return { type: "button", label: block.label, url: block.url };
  });
}

export function draftFromDoc(doc: ContentDoc | null): EmailDraft {
  return {
    subject: doc?.subject ?? "",
    preheader: doc?.preheader ?? "",
    blocks: blocksFromDoc(doc),
    html: doc !== null && doc.mode === "html" ? (doc.html ?? "") : null,
    ownText: doc?.textOverride != null,
    text: doc?.textOverride ?? "",
  };
}

const blockKey = (block: EditorBlock) =>
  block.kind === "button" ? `button:${block.label}\u0000${block.url}` : `${block.kind}:${block.text}`;

/** Semantic equality: the body that is not in use never counts. */
export function sameDraft(a: EmailDraft, b: EmailDraft): boolean {
  if (a.subject !== b.subject || a.preheader !== b.preheader) return false;
  if ((a.html === null) !== (b.html === null)) return false;
  if (a.html !== null) {
    if (a.html !== b.html) return false;
  } else {
    const ak = a.blocks.map(blockKey);
    const bk = b.blocks.map(blockKey);
    if (ak.length !== bk.length || ak.some((key, i) => key !== bk[i])) return false;
  }
  return a.ownText === b.ownText && (!a.ownText || a.text === b.text);
}

/** A refusal that means "the source revision moved" (CAS). */
function isConflict(err: unknown): boolean {
  return (
    err instanceof ApiError && (err.status === 409 || /\bstale\b/i.test(err.message))
  );
}

/**
 * Inline editing state for the Email tab. It lives above the tab
 * panel so unsaved edits survive a tab switch. The saved doc is the
 * truth: a newer doc replaces a clean draft, but while the draft has
 * unsaved edits the local text is kept and flagged stale, and the save
 * stays pinned to the revision the edits began on — a drift refuses
 * as a conflict, never an overwrite.
 */
export function useEmailDraft(
  scope: AudienceScope,
  campaignId: string,
  doc: ContentDoc | null,
  onDoc: (doc: ContentDoc) => void,
) {
  const [draft, setDraft] = useState<EmailDraft>(() => draftFromDoc(doc));
  const [baseline, setBaseline] = useState<EmailDraft>(draft);
  const [source, setSource] = useState(doc?.revision ?? 0);
  const [stale, setStale] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const dirty = doc !== null && !sameDraft(draft, baseline);
  const live = useRef({ dirty, doc });
  live.current = { dirty, doc };

  const identity = doc === null ? "none" : `${doc.revision}:${doc.contentDigest}`;
  useEffect(() => {
    if (doc === null) return;
    if (live.current.dirty) {
      setStale(doc.revision !== source);
      return;
    }
    const next = draftFromDoc(doc);
    setDraft(next);
    setBaseline(next);
    setSource(doc.revision);
    setStale(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [identity]);

  const patch = (change: Partial<EmailDraft>) => setDraft((prev) => ({ ...prev, ...change }));

  const discard = () => {
    const next = draftFromDoc(doc);
    setDraft(next);
    setBaseline(next);
    setSource(doc?.revision ?? 0);
    setStale(false);
    setError(null);
  };

  const save = () => {
    setError(null);
    setNote(null);
    try {
      checkCampaignId(campaignId);
      if (draft.html !== null) {
        checkContent(draft.subject, draft.preheader, [{ type: "paragraph", text: "x" }]);
        if (draft.html.trim() === "") {
          throw new ApiError("paste the HTML body, or discard to keep the blocks", 400);
        }
      } else {
        checkContent(draft.subject, draft.preheader, blocksToGrammar(draft.blocks));
      }
      if (draft.ownText && draft.text.trim() === "") {
        throw new ApiError("write your plain text, or untick Write my own", 400);
      }
    } catch (err: unknown) {
      setError(friendlyCampaignError(err));
      return;
    }
    const common = {
      campaignId,
      subject: draft.subject,
      preheader: draft.preheader,
      ...(draft.ownText ? { text: draft.text } : {}),
      // Pinned to the revision the edits began on, not whatever is
      // current: a concurrent Apply must conflict.
      ...(source > 0 ? { expectedRevision: source } : {}),
    };
    setSaving(true);
    void contentClient
      .save(
        scope,
        draft.html !== null
          ? { ...common, html: draft.html }
          : { ...common, blocks: blocksToGrammar(draft.blocks) },
      )
      .then((value) => {
        const next = parseContentDoc(value);
        const synced = draftFromDoc(next);
        setDraft(synced);
        setBaseline(synced);
        setSource(next.revision);
        setStale(false);
        onDoc(next);
        setNote(`Saved v${next.revision} — approval reset. The preview below is the host render.`);
      })
      .catch((err: unknown) => {
        // CAS conflict: keep the local edits and say what happened.
        setError(
          isConflict(err)
            ? `Not saved — the email changed to a newer version since you started editing (v${source}). Your edits are kept here; Discard to reload the latest, or copy them out first.`
            : friendlyCampaignError(err),
        );
      })
      .finally(() => setSaving(false));
  };

  return { draft, patch, dirty, stale, source, saving, error, note, save, discard };
}

export type EmailDraftApi = ReturnType<typeof useEmailDraft>;
