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

export function blocksFromProposal(blocks: CampaignBlock[]): EditorBlock[] {
  return blocks.map((block) => {
    const editorBlock = newBlock(block.type);
    return block.type === "button"
      ? { ...editorBlock, label: block.label, url: block.url }
      : { ...editorBlock, text: block.text };
  });
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
  const [reloading, setReloading] = useState(false);
  const [needsLatestReload, setNeedsLatestReload] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  /** CAD-1146: undo history for structural block ops (add/move/delete).
   *  Keystroke text edits never push — they ride `patch` directly — so
   *  Undo restores the last block add/move/delete exactly. The history
   *  is UI-local: saves, discards and clean doc syncs reset it. */
  const blockHistory = useRef<EditorBlock[][]>([]);

  // A proposal can seed the revision-0 draft before any saved document
  // exists. Changes there are still dirty and must be explicitly saved.
  const dirty = !sameDraft(draft, baseline);
  const sessionKey = `${scope.installId}:${scope.contextId}:${campaignId}`;
  const live = useRef({ dirty, doc, sessionKey });
  live.current = { dirty, doc, sessionKey };
  const requestEpoch = useRef(0);
  const editEpoch = useRef(0);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      requestEpoch.current += 1;
    };
  }, [sessionKey]);

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
    setNeedsLatestReload(false);
    blockHistory.current = [];
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [identity]);

  const patch = (change: Partial<EmailDraft>) => {
    editEpoch.current += 1;
    setDraft((prev) => ({ ...prev, ...change }));
  };

  /** Structural block change (add/move/delete): undoable. */
  const setBlocks = (blocks: EditorBlock[]) => {
    blockHistory.current = [...blockHistory.current.slice(-29), draft.blocks];
    patch({ blocks });
  };

  /** Restore the blocks from before the last structural change. */
  const undoBlocks = () => {
    const prev = blockHistory.current.pop();
    if (prev !== undefined) patch({ blocks: prev });
  };

  const canUndo = blockHistory.current.length > 0;

  const discard = () => {
    if (saving || reloading) return;
    if (!needsLatestReload && !stale && (doc?.revision ?? 0) === source) {
      const next = draftFromDoc(doc);
      setDraft(next);
      setBaseline(next);
      setSource(doc?.revision ?? 0);
      setStale(false);
      setNeedsLatestReload(false);
      setError(null);
      setNote(null);
      blockHistory.current = [];
      return;
    }

    const epoch = ++requestEpoch.current;
    const editsAtStart = editEpoch.current;
    const requestedSession = sessionKey;
    setReloading(true);
    setError(null);
    setNote(null);
    void contentClient.show(scope, campaignId)
      .then((value) => {
        const latest = parseContentDoc(value);
        if (!mounted.current || live.current.sessionKey !== requestedSession || requestEpoch.current !== epoch) return;
        if (latest.campaignId !== campaignId || latest.revision <= source) {
          throw new ApiError("the latest saved email could not be confirmed; your edits are kept", 409);
        }
        onDoc(latest);
        if (editEpoch.current !== editsAtStart) {
          setError("The draft changed while the latest version was loading. Your edits are kept; discard again to reload.");
          return;
        }
        const next = draftFromDoc(latest);
        setDraft(next);
        setBaseline(next);
        setSource(latest.revision);
        setStale(false);
        setNeedsLatestReload(false);
        setError(null);
        setNote(`Reloaded latest saved version v${latest.revision}.`);
        blockHistory.current = [];
      })
      .catch((err: unknown) => {
        if (!mounted.current || live.current.sessionKey !== requestedSession || requestEpoch.current !== epoch) return;
        setError(`Could not reload the latest saved email. Your edits are kept. ${friendlyCampaignError(err)}`);
      })
      .finally(() => {
        if (mounted.current && live.current.sessionKey === requestedSession && requestEpoch.current === epoch) {
          setReloading(false);
        }
      });
  };

  const save = () => {
    if (saving || reloading) return;
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
        setNeedsLatestReload(false);
        requestEpoch.current += 1;
        blockHistory.current = [];
        onDoc(next);
        setNote(`Saved v${next.revision} — approval reset. The preview below is the host render.`);
      })
      .catch((err: unknown) => {
        if (!isConflict(err)) {
          setError(friendlyCampaignError(err));
          return;
        }
        // A CAS refusal is evidence the cached doc is stale. Fetch it for
        // the page, but never replace the operator's local draft here.
        setNeedsLatestReload(true);
        setStale(true);
        const epoch = ++requestEpoch.current;
        const requestedSession = sessionKey;
        setError(`Not saved — the email changed after editing began (v${source}). Your edits are kept while the latest version loads.`);
        void contentClient.show(scope, campaignId)
          .then((value) => {
            const latest = parseContentDoc(value);
            if (!mounted.current || live.current.sessionKey !== requestedSession || requestEpoch.current !== epoch) return;
            if (latest.campaignId !== campaignId || latest.revision <= source) {
              throw new ApiError("the server did not return a newer saved version", 409);
            }
            onDoc(latest);
            setNeedsLatestReload(true);
            setStale(true);
            setError(`Not saved — the saved email is now v${latest.revision}. Your edits are kept; choose Discard to reload the latest, or copy them out first.`);
          })
          .catch((refreshError: unknown) => {
            if (!mounted.current || live.current.sessionKey !== requestedSession || requestEpoch.current !== epoch) return;
            setError(`Not saved — the email changed after editing began (v${source}), but the latest version could not be loaded. Your edits are kept; retry Discard to reload. ${friendlyCampaignError(refreshError)}`);
          });
      })
      .finally(() => setSaving(false));
  };

  return { draft, patch, setBlocks, undoBlocks, canUndo, dirty, stale, source, saving, reloading, error, note, save, discard };
}

export type EmailDraftApi = ReturnType<typeof useEmailDraft>;
