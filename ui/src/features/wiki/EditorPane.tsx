import { useEffect, useId, useRef, useState } from "react";
import type { Route } from "../../lib/router";
import { sessionStore } from "../../lib/draft";
import { privateSessionGeneration } from "../../lib/privateSession";
import { navigate } from "../../lib/useLocation";
import { useTranslate } from "../../lib/locale";
import { PageText } from "./PagePane";
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
  const t = useTranslate();
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
    const generation = privateSessionGeneration();
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
      if (generation === privateSessionGeneration() && storage && pendingDraft.current) stashDraft(storage, pendingDraft.current);
      pendingDraft.current = null;
    };
  }, [path]);

  const change = (value: string) => {
    if (readOnly || savingRef.current) return;
    setText(value);
    setDirty(true);
    const storage = sessionStore();
    const generation = privateSessionGeneration();
    window.clearTimeout(stashTimer.current);
    pendingDraft.current = { path, text: value, baseRev: baseRevRef.current, at: Date.now() };
    stashTimer.current = window.setTimeout(() => {
      if (generation === privateSessionGeneration() && storage && pendingDraft.current) stashDraft(storage, pendingDraft.current);
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
    const generation = privateSessionGeneration();
    if (storage) stashDraft(storage, { path, text, baseRev: baseRevRef.current, at: Date.now() });
    try {
      const page = await wiki.save(path, text, baseRevRef.current);
      if (generation !== privateSessionGeneration()) return;
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
      if (generation !== privateSessionGeneration()) return;
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
    const generation = privateSessionGeneration();
    try {
      const fresh = await wiki.file(path);
      if (generation !== privateSessionGeneration()) return;
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
      <WikiToolbar path={path} navHref={navHref} label={t("Editing")} actions={<>
          <span className="wk-save-state" role="status">{saving ? t("Saving…") : readOnly ? t("Read-only") : dirty ? t("Unsaved changes") : t("No changes")}</span>
          <Button href={browseHref} disabled={saving}>{t("Back to page")}</Button>
          <Button
            variant="primary"
            onClick={() => void save()}
            loading={saving}
            disabled={readOnly || !dirty}
            title={readOnly ? t("Sign in with editing access to edit") : undefined}
          >
            {t("Save")}
          </Button>
      </>} />

      {conflict && (
        <div className="wk-conflict" role="alert">
          <IconWarning size={13} />
          <span className="min-w-0">{conflictText(conflict)}</span>
          <Button size="sm" onClick={() => setShowDiff((s) => !s)}>{showDiff ? t("Hide comparison") : t("Compare changes")}</Button>
          <Button size="sm" onClick={() => void reload()}>{t("Discard draft and reload")}</Button>
        </div>
      )}

      {showDiff && serverText != null && (
        <div className="wk-diffwrap">
          <div className="slabel wk-difflabel">
            {t("your draft vs the server")} · +{counts.added} −{counts.removed}
          </div>
          <DiffLines lines={diff} />
        </div>
      )}

      <div className="wk-editor-views" role="group" aria-label={t("Editor view")}>
        <button type="button" aria-pressed={editorView === "write"} aria-controls={sourceId} onClick={() => setEditorView("write")}>{t("Write")}</button>
        <button type="button" aria-pressed={editorView === "preview"} aria-controls={previewId} onClick={() => setEditorView("preview")}>{t("Preview")}</button>
      </div>

      <div className="wk-esplit" data-view={editorView}>
        <label className="wk-ehead wk-source-head" htmlFor={sourceId}>{t("Markdown")}</label>
        <textarea
          id={sourceId}
          className="wk-esrc"
          aria-label={t("Markdown")}
          value={text}
          spellCheck={false}
          readOnly={readOnly || saving}
          onChange={(e) => change(e.target.value)}
        />
        <div className="wk-ehead wk-preview-head" id={previewLabelId}>{t("Preview")}</div>
        <div id={previewId} className="wk-eprev issue-reader" role="region" aria-labelledby={previewLabelId}>
          <PageText path={path} text={text} />
        </div>
      </div>

      <p className="wk-hint">
        {t("Unsaved drafts stay in this tab when you leave. Use ⌘S or Ctrl+S to save.")}
      </p>
    </>
  );
}
