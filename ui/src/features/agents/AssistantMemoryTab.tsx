import { useCallback, useEffect, useState } from "react";
import {
  api,
  ApiError,
  type MemberAssistantMemoryHistoryEntry,
  type MemberAssistantMemorySnapshot,
} from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import type { ToastMsg } from "../../ui/Toast";

const TOPIC_SLUG_RE = /^[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?$/;

function fileLabel(target: string, t: (s: string) => string): string {
  if (target === "SOUL.md") return "SOUL.md";
  if (target === "USER.md") return "USER.md";
  if (target === "MEMORY.md") return "MEMORY.md";
  if (target.startsWith("memory/")) return target;
  return t(target);
}

function actorLabel(actor: string, t: (s: string) => string): string {
  if (actor.startsWith("users/")) return t("You");
  return t("Assistant");
}

function opLabel(op: string, t: (s: string) => string): string {
  switch (op) {
    case "remember":
      return t("Added fact");
    case "forget":
      return t("Removed fact");
    case "update_soul":
      return t("SOUL updated");
    case "write_topic":
      return t("Topic updated");
    case "member_edit":
      return t("Edited");
    case "undo":
      return t("Undid change");
    default:
      return op;
  }
}

export default function AssistantMemoryTab({
  writable,
  onToast,
}: {
  writable: boolean;
  onToast: (kind: ToastMsg["kind"], text: string) => void;
}) {
  const t = useTranslate();
  const [snapshot, setSnapshot] = useState<MemberAssistantMemorySnapshot | null>(null);
  const [loadState, setLoadState] = useState<"loading" | "ready" | "unsupported" | "failed">("loading");
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string>("MEMORY.md");
  const [draft, setDraft] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [conflict, setConflict] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [undoBusy, setUndoBusy] = useState<string | null>(null);
  const [creatingTopic, setCreatingTopic] = useState(false);
  const [newSlug, setNewSlug] = useState("");
  const [slugError, setSlugError] = useState<string | null>(null);

  const load = useCallback(async (keepDraft = false) => {
    setLoadState((prev) => (prev === "ready" ? "ready" : "loading"));
    setLoadError(null);
    try {
      const next = await api.memberAssistMemoryGet();
      setSnapshot(next);
      setLoadState("ready");
      if (!keepDraft) setDraft(null);
      setConflict(null);
      setSaveError(null);
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 405 || err.status === 501)) {
        setLoadState("unsupported");
      } else {
        setLoadState("failed");
        setLoadError(err instanceof Error ? err.message : String(err));
      }
    }
  }, []);

  useEffect(() => {
    void load(false);
  }, [load]);

  if (loadState === "unsupported") {
    return <p className="rc-note">{t("Your assistant’s memory is not edited here yet.")}</p>;
  }
  if (loadState === "failed" && snapshot === null) {
    return (
      <p className="rc-note" role="alert">
        {t("Memory could not be read.")} {loadError ? `(${loadError})` : ""}{" "}
        <button type="button" className="ap-link" onClick={() => void load(false)}>
          {t("Retry")}
        </button>
      </p>
    );
  }
  if (loadState === "loading" || snapshot === null) {
    return <p className="rc-note">{t("Loading memory…")}</p>;
  }

  const allEntries = [
    ...snapshot.files.map((f) => ({
      target: f.target,
      label: fileLabel(f.target, t),
      text: f.text,
      sha: f.sha,
      lineCount: f.line_count,
      cap: f.cap,
    })),
    ...snapshot.topics.map((topic) => ({
      target: topic.target,
      label: topic.target,
      text: topic.text,
      sha: topic.sha,
      lineCount: topic.line_count,
      cap: undefined,
    })),
  ];

  const active =
    allEntries.find((e) => e.target === selected) ??
    allEntries.find((e) => e.target === "MEMORY.md") ??
    allEntries[0];

  const servedText = active?.text ?? "";
  const editorText = draft ?? servedText;
  const dirty = draft !== null && draft !== servedText;
  const activeLines =
    editorText.trimEnd() === "" ? 0 : editorText.trimEnd().split(/\r?\n/).length;

  const selectTarget = (nextTarget: string) => {
    setSelected(nextTarget);
    setDraft(null);
    setConflict(null);
    setSaveError(null);
  };

  const handleSave = async () => {
    if (!active || !writable || saving) return;
    setSaving(true);
    setConflict(null);
    setSaveError(null);
    try {
      await api.memberAssistMemoryEdit({
        target: active.target,
        content: editorText,
        expected_sha: active.sha,
      });
      await load(false);
      onToast("ok", t("Saved {target}. Takes effect on the next turn.").replace("{target}", active.target));
    } catch (err) {
      if (err instanceof ApiError && (err.status === 409 || err.code === "revision_conflict")) {
        setConflict(t("This file changed on disk since you loaded it. Reload latest or discard your draft."));
      } else {
        setSaveError(err instanceof Error ? err.message : t("Could not save changes."));
      }
    } finally {
      setSaving(false);
    }
  };

  const handleCreateTopic = async () => {
    if (!writable || saving) return;
    const slug = newSlug.trim().toLowerCase();
    if (!TOPIC_SLUG_RE.test(slug) || slug.includes("--")) {
      setSlugError(t("Use 1–48 lowercase letters, digits, or single hyphens."));
      return;
    }
    setSlugError(null);
    const target = `memory/${slug}.md`;
    setSaving(true);
    setSaveError(null);
    try {
      const initial = `# ${slug}\n`;
      await api.memberAssistMemoryEdit({ target, content: initial });
      setCreatingTopic(false);
      setNewSlug("");
      await load(false);
      setSelected(target);
      setDraft(null);
      onToast("ok", t("Created {target}.").replace("{target}", target));
    } catch (err) {
      setSlugError(err instanceof Error ? err.message : t("Could not create topic."));
    } finally {
      setSaving(false);
    }
  };

  const handleUndo = async (entry: MemberAssistantMemoryHistoryEntry) => {
    if (!writable || undoBusy !== null || entry.undone_by || entry.op === "undo") return;
    setUndoBusy(entry.id);
    try {
      await api.memberAssistMemoryUndo(entry.id);
      await load(false);
      onToast("ok", t("Undid change to {target}.").replace("{target}", entry.target));
    } catch (err) {
      onToast("err", err instanceof Error ? err.message : t("Could not undo change."));
    } finally {
      setUndoBusy(null);
    }
  };

  return (
    <div className="ap-mem-grid" data-memory-tab>
      <section className="ap-mem-editor" aria-label={t("Assistant memory files")}>
        <div className="ap-mem-files" role="tablist" aria-label={t("Memory files")}>
          {allEntries.map((entry) => (
            <button
              key={entry.target}
              type="button"
              role="tab"
              aria-selected={active?.target === entry.target}
              className={`ap-mem-pill ${active?.target === entry.target ? "active" : ""}`}
              onClick={() => selectTarget(entry.target)}
            >
              {entry.label}
            </button>
          ))}
          {writable && (
            <button
              type="button"
              className="ap-mem-pill add"
              onClick={() => {
                setCreatingTopic((cur) => !cur);
                setSlugError(null);
              }}
            >
              + {t("New topic")}
            </button>
          )}
        </div>

        {creatingTopic && (
          <div className="ap-mem-new-topic">
            <input
              type="text"
              className="field"
              aria-label={t("Topic slug")}
              placeholder="preferences"
              value={newSlug}
              onChange={(e) => setNewSlug(e.target.value)}
            />
            <Button size="sm" variant="primary" disabled={saving} onClick={() => void handleCreateTopic()}>
              {t("Create")}
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setCreatingTopic(false)}>
              {t("Cancel")}
            </Button>
            {slugError && (
              <small className="ap-mem-err" role="alert">
                {slugError}
              </small>
            )}
          </div>
        )}

        {active && (
          <div className="ap-doc">
            <div className="ap-mem-meta">
              <span className="num">
                {active.target} ·{" "}
                {active.cap
                  ? t("{n} / {cap} preloaded lines").replace("{n}", String(activeLines)).replace("{cap}", String(active.cap))
                  : t("{n} lines").replace("{n}", String(activeLines))}
              </span>
              <span className="ap-doc-muted">{t("Saved edits take effect on your assistant’s next turn.")}</span>
            </div>

            <textarea
              className="field rc-text ap-doc-text"
              aria-label={active.target}
              spellCheck
              value={editorText}
              readOnly={!writable || saving}
              onChange={(e) => {
                setDraft(e.target.value);
                setConflict(null);
                setSaveError(null);
              }}
            />

            {conflict && (
              <div className="agents-banner tone-warn ap-mem-banner" role="alert">
                <span>{conflict}</span>
                <button type="button" className="ap-link" onClick={() => void load(false)}>
                  {t("Reload latest")}
                </button>
              </div>
            )}
            {saveError && (
              <div className="agents-banner tone-fail ap-mem-banner" role="alert">
                <span>{saveError}</span>
              </div>
            )}

            <div className="ap-doc-save">
              <span className="ap-doc-hint">
                {dirty ? t("Unsaved changes") : t("Up to date")}
              </span>
              <span className="ap-doc-spacer" />
              <Button
                size="sm"
                variant="ghost"
                disabled={!dirty || saving}
                onClick={() => {
                  setDraft(null);
                  setConflict(null);
                  setSaveError(null);
                }}
              >
                {t("Discard")}
              </Button>
              <Button
                size="sm"
                variant="primary"
                disabled={!writable || !dirty || saving}
                onClick={() => void handleSave()}
              >
                {saving ? t("Saving…") : t("Save")}
              </Button>
            </div>
          </div>
        )}
      </section>

      <aside className="ap-mem-history" aria-label={t("Memory change history")}>
        <div className="ap-panel-h">
          <h3>{t("Recent changes")}</h3>
          <button type="button" className="ap-link" onClick={() => void load(true)}>
            {t("Refresh")}
          </button>
        </div>
        {snapshot.history.length === 0 ? (
          <p className="rc-note">{t("No changes recorded yet.")}</p>
        ) : (
          <ul className="ap-mem-hlist">
            {snapshot.history.map((entry) => {
              const isUndone = Boolean(entry.undone_by);
              const canUndoEntry = writable && !isUndone && entry.op !== "undo";
              return (
                <li key={entry.id} className="ap-mem-hitem" data-history-id={entry.id}>
                  <div className="ap-mem-hhead">
                    <span className="ap-mem-hop">{opLabel(entry.op, t)}</span>
                    <span className="ap-mem-htarget num">{entry.target}</span>
                    <span className="ap-mem-hactor">{actorLabel(entry.actor, t)}</span>
                  </div>
                  {(entry.added.length > 0 || entry.removed.length > 0) && (
                    <ul className="ap-mem-hdiff num">
                      {entry.added.slice(0, 3).map((line, i) => (
                        <li key={`ha${i}`} className="ctx-diff-add">
                          + {line}
                        </li>
                      ))}
                      {entry.removed.slice(0, 3).map((line, i) => (
                        <li key={`hr${i}`} className="ctx-diff-rem">
                          − {line}
                        </li>
                      ))}
                    </ul>
                  )}
                  <div className="ap-mem-hfoot">
                    <span className="ap-mem-htime num">{entry.at}</span>
                    {isUndone ? (
                      <span className="ap-pill off">{t("Undone")}</span>
                    ) : (
                      canUndoEntry && (
                        <button
                          type="button"
                          className="ap-link"
                          disabled={undoBusy !== null}
                          onClick={() => void handleUndo(entry)}
                        >
                          {undoBusy === entry.id ? t("Undoing…") : t("Undo")}
                        </button>
                      )
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </aside>
    </div>
  );
}
