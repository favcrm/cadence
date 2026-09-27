import { useEffect, useRef, useState } from "react";
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
import { Crumbs, Failure, Loading } from "./shared";

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
  const [text, setText] = useState("");
  const [baseRev, setBaseRev] = useState("");
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
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
    let cancelled = false;
    setStatus("loading");
    setConflict(null);
    setServerText(null);
    setShowDiff(false);
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
      cancelled = true;
      window.clearTimeout(stashTimer.current);
      if (storage && pendingDraft.current) stashDraft(storage, pendingDraft.current);
      pendingDraft.current = null;
    };
  }, [path]);

  const change = (value: string) => {
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
    if (readOnly) {
      onToast("err", "writes are disabled on this board — sign in as the operator to edit.");
      return;
    }
    setSaving(true);
    try {
      const page = await wiki.save(path, text, baseRevRef.current);
      window.clearTimeout(stashTimer.current);
      pendingDraft.current = null;
      const storage = sessionStore();
      if (storage) dropDraft(storage, path);
      setDirty(false);
      if (page?.rev) {
        setBaseRev(page.rev);
      }
      onSaved();
      navigate(browseHref);
    } catch (e) {
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
      setSaving(false);
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
      onToast("ok", "reloaded the server's copy — the draft was dropped");
    } catch (e) {
      onToast("err", `reload failed — ${message(e)}`);
    }
  };

  if (status === "loading") return <Loading what="loading the page…" />;
  if (status === "error") return <Failure what="could not open the page" error={error} />;

  const diff = serverText != null ? lineDiff(serverText, text) : [];
  const counts = diffCounts(diff);

  return (
    <>
      <div className="wk-bar">
        <Crumbs
          path={path}
          hrefFor={(p) => navHref({ screen: "wiki", mode: "browse", path: p || null, query: null })}
        />
        <span className="wk-sep">·</span>
        <span className="wk-editing">editing</span>
        <div className="wk-tools">
          <Button href={browseHref}>Cancel</Button>
          <Button
            variant="primary"
            onClick={() => void save()}
            disabled={saving || readOnly}
            title={readOnly ? "writes are disabled — sign in as the operator" : undefined}
          >
            Save
          </Button>
        </div>
      </div>

      {conflict && (
        <div className="wk-conflict" role="alert">
          <IconWarning size={13} />
          <span className="min-w-0">{conflictText(conflict)}</span>
          <Button size="sm" onClick={() => setShowDiff((s) => !s)}>review diff</Button>
          <Button size="sm" onClick={() => void reload()}>reload</Button>
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

      <div className="wk-esplit">
        <div className="wk-ehead">
          <span>source</span>
          <span>preview</span>
        </div>
        <textarea
          className="wk-esrc"
          value={text}
          spellCheck={false}
          readOnly={readOnly}
          aria-label="markdown source"
          onChange={(e) => change(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "s") {
              e.preventDefault();
              void save();
            }
          }}
        />
        <div className="wk-eprev issue-reader">
          <Md text={text} />
        </div>
      </div>

      <p className="wk-hint">
        Drafts stay in this tab until saved. Use ⌘S or Ctrl+S to save.
        {dirty ? " · unsaved changes" : ""}
      </p>
    </>
  );
}
