import { useEffect, useId, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { onPrivateSessionReset } from "../../lib/privateSession";
import Button from "../../ui/Button";
import { Note } from "./shared";
import { sharedCollectionCitations } from "./citations";
import { submitCorrection } from "./knowledge";

/** Reports the visible question and answer only. Tool rows and payloads are never read. */
export default function CorrectionReport({ question, answer }: { question: string; answer: string }) {
  const t = useTranslate();
  const expectedId = useId();
  const reasonId = useId();
  const [open, setOpen] = useState(false);
  const [expected, setExpected] = useState("");
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => onPrivateSessionReset(() => {
    setOpen(false);
    setExpected("");
    setReason("");
    setError(null);
    setDone(null);
  }), []);
  const cited = sharedCollectionCitations(answer);
  // Nothing to correct without a cited shared-collection revision: a plain
  // answer, a private path and a cross-collection citation all offer no action.
  if ("error" in cited) return null;
  const submit = async () => {
    if (busy || expected.trim() === "" || reason.trim() === "") return;
    setBusy(true);
    setError(null);
    try {
      const saved = await submitCorrection({
        path: cited.root,
        question,
        answer,
        expected_answer: expected.trim(),
        reason: reason.trim(),
        citations: cited.citations,
      });
      setDone(saved.id);
      setOpen(false);
    } catch (reasonError) {
      setError(reasonError instanceof Error ? reasonError.message : String(reasonError));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="wk-report">
      {done && <Note>{t("Correction submitted for review")}: {done}</Note>}
      {!open ? (
        <Button onClick={() => setOpen(true)}>{t("Report a correction")}</Button>
      ) : (
        <form className="wk-review card" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
          <h2 className="wk-dialog-title">{t("Report a correction")}</h2>
          <p className="kicker">{t("Only the visible question, answer and shared-collection citations below are sent.")}</p>
          <p><span className="slabel">{t("Reported question")}</span></p>
          <pre className="wk-report-text">{question || t("Unknown")}</pre>
          <p><span className="slabel">{t("Reported answer")}</span></p>
          <pre className="wk-report-text">{answer}</pre>
          <ul>{cited.citations.map((cite) => <li key={`${cite.path}@${cite.revision}`}>{cite.path} @ {cite.revision}{cite.section ? ` #${cite.section}` : ""}{cite.page ? ` page ${cite.page}` : ""}</li>)}</ul>
          <label htmlFor={expectedId}>{t("Expected answer")}</label>
          <textarea id={expectedId} className="field" required rows={3} value={expected} onChange={(event) => setExpected(event.target.value)} />
          <label htmlFor={reasonId}>{t("Member reason")}</label>
          <textarea id={reasonId} className="field" required rows={3} value={reason} onChange={(event) => setReason(event.target.value)} />
          {error && <p className="text-fail" role="alert">{error}</p>}
          <div className="wk-tools">
            <Button type="submit" variant="primary" loading={busy}>{t("Submit correction")}</Button>
            <Button type="button" disabled={busy} onClick={() => setOpen(false)}>{t("Cancel")}</Button>
          </div>
        </form>
      )}
    </div>
  );
}

export function previousQuestion(texts: Array<{ type: string; text: string }>, index: number): string {
  for (let cursor = index - 1; cursor >= 0; cursor--) {
    if (texts[cursor].type === "operator") return texts[cursor].text;
  }
  return "";
}
