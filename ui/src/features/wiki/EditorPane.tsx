import { useEffect, useId, useRef, useState } from "react";
import type { Route } from "../../lib/router";
import { sessionStore } from "../../lib/draft";
import { navigate } from "../../lib/useLocation";
import Md from "../../ui/Md";
import { IconWarning } from "../../ui/icons";
import { wiki } from "./api";
import {
  conflictFrom,
  conflictText,
  dropDraft,
  readDraft,
  stashDraft,
  type ConflictInfo,
  type WikiDraft,
} from "./editor";
import { diffCounts, lineDiff, type DiffLine } from "./diff";
import Button from "../../ui/Button";
import { Failure, Loading, WikiToolbar } from "./shared";

/**
 * The editor (CAD-581): source on the left, live preview on the right.
 * Save sends `if_rev`; a refusal is the conflict banner — the draft stays
 * in sessionStorage, and reload takes the server's copy instead.
 */

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function DiffLines({ lines }: { lines: DiffLine[] }) {
  return (
    <div className="wk-diff">
      {lines.map((line, i) => (
        <div key={i} className={`wk-dl ${line.kind}`}>
          {line.text || " "}
        </div>
      ))}
    </div>
  );
}

export default function EditorPane({
  path,
  navHref,
  readOnly,
  onToast,
  onSaved,
}: {
  path: string;
  navHref: (route: Route) => string;
  readOnly: boolean;
  onToast: (kind: "ok" | "err" | "warn", text: string) => void;
  onSaved: () => void;
}) {
  const sourceId = useId();
  const previewId = useId();
  const previewLabelId = useId();
  const [editorView, setEditorView] = useState<"write" | "preview">("write");
  const [text, setText] = useState("");
  const [baseRev, setBaseRev] = useState("");
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const savingRef = useRef(false);
  const activeRef = useRef(false);
  const [dirty, setDirty] = useState(false);
  const [conflict, setConflict] = useState<ConflictInfo | null>(null);
  const [serverText, setServerText] = useState<string | null>(null);
  const [showDiff, setShowDiff] = useState(false);
  const baseRevRef = useRef(baseRev);
  baseRevRef.current = baseRev;
  const stashTimer = useRef(0);
  const pendingDraft = useRef<WikiDraft | null>(null);

  const browseHref = navHref({ screen: "wiki", mode: "browse", path, query: null });

  useEffect(() => {
    activeRef.current = true;
    let cancelled = false;
    setStatus("loading");
    setConflict(null);
    setServerText(null);
    setShowDiff(false);
    setEditorView("write");
    const storage = sessionStore();
    const draft = storage ? readDraft(storage, path) : null;
    wiki
      .file(path)
      .then((page) => {
        if (cancelled) return;
        const server = page.text ?? "";
        if (draft && draft.text !== server) {
          setText(draft.text);
          setBaseRev(draft.baseRev || page.rev || "");
          setDirty(true);
        } else {
          setText(server);
          setBaseRev(page.rev ?? "");
          setDirty(false);
          if (draft && storage) dropDraft(storage, path);
        }
        setStatus("ready");
      })
      .catch((e) => {
        if (cancelled) return;
        setStatus("error");
        setError(message(e));
      });
    return () => {
      activeRef.current = false;
      cancelled = true;
      window.clearTimeout(stashTimer.current);
      if (storage && pendingDraft.current) stashDraft(storage, pendingDraft.current);
      pendingDraft.current = null;
    };
  }, [path]);

  const change = (value: string) => {
    if (readOnly || savingRef.current) return;
    setText(value);
    setDirty(true);
    const storage = sessionStore();
    window.clearTimeout(stashTimer.current);
    pendingDraft.current = { path, text: value, baseRev: baseRevRef.current, at: Date.now() };
    stashTimer.current = window.setTimeout(() => {
      if (storage && pendingDraft.current) stashDraft(storage, pendingDraft.current);
      pendingDraft.current = null;
    }, 400);
  };

  const save = async () => {
    if (savingRef.current || !dirty || status !== "ready") return;
    if (readOnly) {
      onToast("err", "writes are disabled on this board — sign in as the operator to edit.");
      return;
    }
    savingRef.current = true;
    setSaving(true);
    window.clearTimeout(stashTimer.current);
    pendingDraft.current = null;
    const storage = sessionStore();
    if (storage) stashDraft(storage, { path, text, baseRev: baseRevRef.current, at: Date.now() });
    try {
      const page = await wiki.save(path, text, baseRevRef.current);
      // A newer editor may have written another draft while this request ran.
      if (storage && readDraft(storage, path)?.text === text) dropDraft(storage, path);
      if (!activeRef.current) return;
      setDirty(false);
      if (page?.rev) {
        setBaseRev(page.rev);
      }
      onSaved();
      navigate(browseHref);
    } catch (e) {
      if (!activeRef.current) return;
      const refused = conflictFrom(e);
      if (refused) {
        setConflict(refused);
        try {
          const fresh = await wiki.file(path);
          setServerText(fresh.text ?? "");
        } catch {
          // The banner stands without a diff — the save can still be retried.
        }
      } else {
        onToast("err", `save failed — ${message(e)}`);
      }
    } finally {
      savingRef.current = false;
      if (activeRef.current) setSaving(false);
    }
  };

  const reload = async () => {
    try {
      const fresh = await wiki.file(path);
      window.clearTimeout(stashTimer.current);
      pendingDraft.current = null;
      const storage = sessionStore();
      if (storage) dropDraft(storage, path);
      setText(fresh.text ?? "");
      setBaseRev(fresh.rev ?? "");
      setConflict(null);
      setShowDiff(false);
      setDirty(false);
      onToast("ok", "Latest version loaded. Your draft was discarded.");
    } catch (e) {
      onToast("err", `reload failed — ${message(e)}`);
    }
  };

  // Save remains available when the mobile preview has keyboard focus.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key === "s") {
        event.preventDefault();
        void save();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  });

  if (status === "loading") return <Loading what="loading the page…" />;
  if (status === "error") return <Failure what="could not open the page" error={error} />;

  const diff = serverText != null ? lineDiff(serverText, text) : [];
  const counts = diffCounts(diff);

  return (
    <>
      <WikiToolbar path={path} navHref={navHref} label="Editing" actions={<>
          <span className="wk-save-state" role="status">{saving ? "Saving…" : readOnly ? "Read-only" : dirty ? "Unsaved changes" : "No changes"}</span>
          <Button href={browseHref} disabled={saving}>Back to page</Button>
          <Button
            variant="primary"
            onClick={() => void save()}
            loading={saving}
            disabled={readOnly || !dirty}
            title={readOnly ? "Sign in with editing access to edit" : undefined}
          >
            Save
          </Button>
      </>} />

      {conflict && (
        <div className="wk-conflict" role="alert">
          <IconWarning size={13} />
          <span className="min-w-0">{conflictText(conflict)}</span>
          <Button size="sm" onClick={() => setShowDiff((s) => !s)}>{showDiff ? "Hide comparison" : "Compare changes"}</Button>
          <Button size="sm" onClick={() => void reload()}>Discard draft and reload</Button>
        </div>
      )}

      {showDiff && serverText != null && (
        <div className="wk-diffwrap">
          <div className="slabel wk-difflabel">
            your draft vs the server · +{counts.added} −{counts.removed}
          </div>
          <DiffLines lines={diff} />
        </div>
      )}

      <div className="wk-editor-views" role="group" aria-label="Editor view">
        <button type="button" aria-pressed={editorView === "write"} aria-controls={sourceId} onClick={() => setEditorView("write")}>Write</button>
        <button type="button" aria-pressed={editorView === "preview"} aria-controls={previewId} onClick={() => setEditorView("preview")}>Preview</button>
      </div>

      <div className="wk-esplit" data-view={editorView}>
        <label className="wk-ehead wk-source-head" htmlFor={sourceId}>Markdown</label>
        <textarea
          id={sourceId}
          className="wk-esrc"
          aria-label="Markdown"
          value={text}
          spellCheck={false}
          readOnly={readOnly || saving}
          onChange={(e) => change(e.target.value)}
        />
        <div className="wk-ehead wk-preview-head" id={previewLabelId}>Preview</div>
        <div id={previewId} className="wk-eprev issue-reader" role="region" aria-labelledby={previewLabelId}>
          <Md text={text} />
        </div>
      </div>

      <p className="wk-hint">
        Unsaved drafts stay in this tab when you leave. Use ⌘S or Ctrl+S to save.
      </p>
    </>
  );
}
