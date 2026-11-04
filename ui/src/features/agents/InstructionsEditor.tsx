import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { useTranslate } from "../../lib/localeText";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import DrawerShell from "../app-shell/shared/DrawerShell";
import { isDirty, sizeNote, textProblem, type ContextLine } from "./instructionsDoc";
import type { InstructionsDoc } from "./useInstructionsDoc";

/**
 * The Instructions tab: one Markdown document that fills the page. Edit and
 * Preview show the same draft. The textarea stays mounted behind Preview so the
 * browser keeps its caret, selection and scroll, and the position is also kept
 * on the page so a tab trip gives it back.
 */
export default function InstructionsEditor({
  doc,
  writable,
  resetTo,
  contextHeading,
  contextLines,
  contextNote,
}: {
  doc: InstructionsDoc;
  writable: boolean;
  /** The role's own text, offered as a quiet reset when the draft differs. */
  resetTo: string | null;
  /** What the lines are the start of; true only where a session really loads it. */
  contextHeading: string;
  contextLines: ContextLine[];
  contextNote: ReactNode;
}) {
  const t = useTranslate();
  const kind = doc.kind ?? "native";
  const [mode, setMode] = useState<"edit" | "preview">("edit");
  const [context, setContext] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);
  const served = doc.served ?? "";
  const text = doc.draft ?? served;
  const dirty = isDirty(kind, doc.draft, served);
  const pending = doc.phase === "conflict" || doc.phase === "unconfirmed";
  const saving = doc.phase === "saving";
  // While a save's outcome is unknown the text stays as it was sent, so a
  // check judges what was actually written.
  const editable = writable && (doc.phase === "idle" || doc.phase === "conflict");
  const problem = dirty && doc.phase === "idle" ? textProblem(kind, text) : null;
  const unsaved = dirty || pending;
  // Whatever is unsaved stays on screen, even while the board can't take a
  // write: a missing Save must never read as "saved".
  const view = writable || unsaved ? mode : "edit";
  const hint = !writable
    ? doc.phase === "unconfirmed"
      ? "Not confirmed. The board isn’t answering; check once it does."
      : "Not saved. The board isn’t answering, so saving is paused."
    : doc.phase === "unconfirmed"
      ? "Not confirmed: the last save may or may not have reached the board. The text is locked until you check."
      : doc.phase === "conflict"
        ? "Someone else changed this. Your draft is kept."
        : problem;

  const remember = () => {
    const el = area.current;
    if (el) doc.selection.current = { start: el.selectionStart, end: el.selectionEnd, top: el.scrollTop };
  };
  // Back on Edit, or back on the tab: give the caret and scroll back. A
  // textarea that stayed mounted already has them; a new one gets them here.
  useLayoutEffect(() => {
    const el = area.current;
    if (!el || mode !== "edit") return;
    const { start, end, top } = doc.selection.current;
    if (el.selectionStart !== start || el.selectionEnd !== end) el.setSelectionRange(start, end);
    el.scrollTop = top;
  }, [mode, doc.selection]);

  const toPreview = () => {
    remember();
    setMode("preview");
  };
  const toEdit = () => {
    setMode("edit");
    requestAnimationFrame(() => area.current?.focus({ preventScroll: true }));
  };

  return (
    <div className="ap-doc">
      <div className="ap-doc-bar">
        {/* A reader has nothing to edit, so Edit would be inert. */}
        {(writable || unsaved) && (
          <div className="rc-cats" role="group" aria-label={t("Instructions view")}>
            <button type="button" className="rc-cat" aria-pressed={mode === "edit"} onClick={toEdit}>
              {t("Edit")}
            </button>
            <button type="button" className="rc-cat" aria-pressed={mode === "preview"} onClick={toPreview}>
              {t("Preview")}
            </button>
          </div>
        )}
        <span className="ap-doc-spacer" />
        {editable && resetTo !== null && text !== resetTo && (
          <button type="button" className="ap-link" onClick={() => doc.setDraft(resetTo)}>
            {t("Reset to role")}
          </button>
        )}
        <button type="button" className="ap-link" onClick={() => setContext(true)}>
          {t("Context")}
        </button>
      </div>
      <textarea
        ref={area}
        className="field rc-text ap-doc-text"
        aria-label={t("Instructions")}
        hidden={view !== "edit"}
        spellCheck
        value={text}
        readOnly={!editable}
        onChange={(event) => doc.setDraft(event.target.value)}
        onBlur={remember}
        onSelect={remember}
        onScroll={remember}
      />
      {view === "preview" && (
        <div className="ap-doc-preview issue-reader text-secondary text-ink-300" tabIndex={0} aria-label={t("Instructions preview")}>
          {text.trim() ? <Md text={text} /> : <p className="rc-note">{t("Nothing written yet.")}</p>}
        </div>
      )}
      {unsaved && (
        <div className="ap-doc-save">
          <small className="ap-doc-hint" role={hint ? "alert" : undefined}>
            {hint ? t(hint) : (sizeNote(kind, text) ?? "")}
            {!hint && kind === "native" && doc.phase === "idle" && (
              <span className="ap-doc-hint-note">
                {t("This board keeps no revision, so saving replaces the text it holds.")}
              </span>
            )}
          </small>
          <span className="ap-doc-spacer" />
          {doc.phase === "conflict" ? (
            <>
              <Button size="sm" disabled={!writable || saving} onClick={doc.useTheirs}>
                {t("Discard mine, use theirs")}
              </Button>
              <Button variant="primary" size="sm" disabled={!writable || saving} onClick={doc.saveMine}>
                {t("Save mine")}
              </Button>
            </>
          ) : doc.phase === "unconfirmed" ? (
            <>
              <Button variant="ghost" size="sm" disabled={!writable} onClick={doc.discard}>
                {t("Discard")}
              </Button>
              <Button variant="primary" size="sm" disabled={!writable} onClick={doc.check}>
                {t("Check now")}
              </Button>
            </>
          ) : (
            <>
              <Button variant="ghost" size="sm" disabled={saving} onClick={doc.discard}>
                {t("Discard")}
              </Button>
              <Button variant="primary" size="sm" disabled={!writable || saving || problem !== null} onClick={doc.save}>
                {saving ? t("Saving…") : t("Save")}
              </Button>
            </>
          )}
        </div>
      )}
      {context && (
        <DrawerShell
          kind="agent-item"
          label={t("Context")}
          title={t("Context")}
          tabs={[
            {
              id: "context",
              label: t("Context"),
              panel: (
                <div className="ap-body">
                  <section>
                    <h4 className="ap-doc-h">{t(contextHeading)}</h4>
                    <ul className="ap-doc-lines">
                      {contextLines.map((line) => (
                        <li key={line.name}>
                          <span>{t(line.name)}</span>
                          <span className="ap-doc-muted">{t(line.size)}</span>
                        </li>
                      ))}
                    </ul>
                  </section>
                  <section>
                    <h4 className="ap-doc-h">{t("Loaded when needed")}</h4>
                    {contextNote}
                  </section>
                </div>
              ),
            },
          ]}
          onClose={() => setContext(false)}
        />
      )}
    </div>
  );
}
