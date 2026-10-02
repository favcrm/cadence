import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import DataTable from "./shared/DataTable";
import Field from "./shared/Field";
import { ErrorNotice, Notice } from "./shared/States";
import {
  buildCsvDecisions,
  csvActions,
  csvApplyCount,
  csvDecisionsReady,
  csvPlanChoice,
  newImportRequestId,
  type CsvPreview,
  type CsvRowChoice,
} from "./csvClient";
import { csvDecisionsDigest, toWireDecisions } from "./csvDigest";
import { friendlyError, viewProfile } from "./customerProfile";
import type { HostScope } from "./hostActions";

/**
 * The chat-bounded CSV import (CAD-1016): a small pasted customer list is
 * previewed read-only into an exact create/update/skip/error plan; the
 * operator reviews the rows, resolves `needs_revision`, then Confirms.
 * The confirm mints the host-held one-use `confirm_token` (`csv-confirm`,
 * bound to the byte token + request id + decisions digest — never agent
 * text) and hands the immutable import intent to the next scoped agent
 * turn as a scoped chat message. The agent's turn then redeems the
 * receipt via `csv-assistant-import` — the operator's own connection is
 * the only thing that can confirm; a scoped message alone is not consent.
 *
 * The intent rides the bounded chat message: total serialized size is
 * checked against the real message bound before the confirm is even
 * minted — a list that does not fit is refused up front with a clear
 * message, never silently truncated. No token is ever shown to copy, no
 * CLI is run by the user, and the CSV/decisions never reach a log or an
 * error string. Cancel writes nothing.
 */

/** The serialized scoped-message bound — the chat transport's text cap
 *  minus the envelope + scope field headroom. A paste whose intent will
 *  not fit under this is refused before the confirm is minted. */
const CHAT_INTENT_MAX_BYTES = 40 * 1024;

const CSV_HINT =
  "record_id,display_name,email,phone,tags,source,consent_email,consent_sms,expected_revision";

export default function ChatCsvImport({
  scope,
  canWrite,
  onSendIntent,
}: {
  scope: HostScope;
  canWrite: boolean;
  /** Sends the serialized import intent as a scoped chat message to the
   *  next agent turn. The caller attaches the verified scope and posts. */
  onSendIntent: (intent: {
    request_id: string;
    preview_token: string;
    decisions_digest: string;
    confirm_token: string;
    csv_text: string;
    decisions?: { row: number; action: string; expected_revision?: number }[];
  }) => Promise<string | null>;
}) {
  const [csvText, setCsvText] = useState("");
  const [preview, setPreview] = useState<CsvPreview | null>(null);
  const [choices, setChoices] = useState<Map<number, CsvRowChoice>>(new Map());
  const [revisions, setRevisions] = useState<Map<number, string>>(new Map());
  const [requestId, setRequestId] = useState(newImportRequestId);
  const [pending, setPending] = useState<"preview" | "confirm" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sent, setSent] = useState<string | null>(null);
  const live = useRef<{ mounted: boolean; scope: string }>({ mounted: true, scope: "" });
  live.current.scope = `${scope.installId}:${scope.contextId}`;
  useEffect(() => {
    live.current.mounted = true;
    return () => {
      live.current.mounted = false;
    };
  }, []);

  const resetPlan = () => {
    setPreview(null);
    setChoices(new Map());
    setRevisions(new Map());
    setError(null);
    setSent(null);
  };

  const runPreview = () => {
    if (csvText.trim() === "") {
      setError("Paste a small customer CSV first — a header row plus one row per customer.");
      return;
    }
    setPending("preview");
    setError(null);
    setSent(null);
    const scopeKey = live.current.scope;
    const text = csvText;
    void csvActions
      .preview(scope, text)
      .then((plan) => {
        if (!live.current.mounted || live.current.scope !== scopeKey) return;
        setPreview(plan);
        setRequestId(newImportRequestId());
        setChoices(new Map(plan.rows.map((row) => [row.row, csvPlanChoice(row)])));
        setRevisions(new Map());
      })
      .catch((e: unknown) => {
        if (live.current.mounted && live.current.scope === scopeKey) setError(friendlyError(e));
      })
      .finally(() => {
        if (live.current.mounted && live.current.scope === scopeKey) setPending(null);
      });
  };

  const confirmPlan = async () => {
    if (preview === null) return;
    const scopeKey = live.current.scope;
    const decisions = buildCsvDecisions(preview.rows, choices, revisions);
    const wireDecisions = toWireDecisions(decisions);
    // Serialize the immutable intent for the agent's next turn before
    // minting the confirm — if it cannot fit the chat message bound, the
    // confirm is never minted and nothing is silently truncated. The
    // decisions ride the message in the daemon's snake_case wire form so
    // the agent can recompute the bound digest and pass them on verbatim.
    const intentCore = {
      // `kind` marks the scoped message as an immutable import intent
      // the assistant turn recognises and redeems — not free chat text.
      kind: "crm-csv-import",
      request_id: requestId,
      preview_token: preview.previewToken,
      decisions_digest: csvDecisionsDigest(decisions),
      csv_text: csvText,
      ...(wireDecisions.length > 0 ? { decisions: wireDecisions } : {}),
    };
    // confirm_token is appended after mint; size it with a placeholder
    // of the same bounded form so the bound is checked against the real
    // message the agent's turn will receive.
    const probe = { ...intentCore, confirm_token: "confirm-" + "0".repeat(32) };
    const bytes = new TextEncoder().encode(JSON.stringify(probe)).length;
    if (bytes > CHAT_INTENT_MAX_BYTES) {
      setError(
        `This list is too large to hand to the assistant through chat (${Math.round(bytes / 1024)} KiB over the ${Math.round(CHAT_INTENT_MAX_BYTES / 1024)} KiB bound). Import a smaller batch — split the CSV and confirm each part.`,
      );
      return;
    }
    setPending("confirm");
    setError(null);
    try {
      const receipt = await csvActions.confirm(
        scope,
        preview.previewToken,
        requestId,
        intentCore.decisions_digest,
      );
      if (!live.current.mounted || live.current.scope !== scopeKey) return;
      const sendError = await onSendIntent({ ...intentCore, confirm_token: receipt.confirmToken });
      if (!live.current.mounted || live.current.scope !== scopeKey) return;
      if (sendError === null) {
        setSent("Confirmed — the import intent went to the assistant's scoped turn. It applies the reviewed rows; this board shows the outcome.");
        resetPlan();
      } else {
        // The confirm minted but the hand-off send failed — say so
        // honestly; the operator retries the same request id, which the
        // daemon replays/redeems idempotently.
        setError(`Confirmed, but the assistant turn did not start: ${sendError}. Retry the same preview to hand it off again.`);
      }
    } catch (e: unknown) {
      if (live.current.mounted && live.current.scope === scopeKey) {
        setError(friendlyError(e));
      }
    } finally {
      if (live.current.mounted && live.current.scope === scopeKey) setPending(null);
    }
  };

  if (!canWrite) {
    return (
      <Notice className="mt-2" state="read-only">
        {scope.contextId === ""
          ? "Pick a customer context before importing through chat."
          : "Read-only view. Customer imports through the assistant are disabled here."}
      </Notice>
    );
  }

  const applyCount = preview === null ? 0 : csvApplyCount(preview.rows, choices);

  return (
    <section aria-label="Import customers via chat" className="grid gap-2" data-chat-import>
      <p className="text-label text-ink-400">
        Paste a small customer CSV — the assistant previews it, you review each row, then a Confirm
        hands the approved plan to its next turn. Nothing imports until you Confirm.
      </p>
      {preview === null && (
        <form
          className="grid gap-2"
          aria-label="CSV source"
          onSubmit={(e) => {
            e.preventDefault();
            runPreview();
          }}
        >
          <Field label="CSV text — header plus one row per customer" id="chat-csv-text" className="crm-field">
            {(c) => (
              <textarea
                id={c.id}
                className="field"
                rows={6}
                value={csvText}
                onChange={(e) => {
                  setCsvText(e.target.value);
                  if (preview !== null) resetPlan();
                }}
                disabled={pending !== null}
                spellCheck={false}
                autoComplete="off"
                placeholder={CSV_HINT}
              />
            )}
          </Field>
          {error !== null && <ErrorNotice bare>{error}</ErrorNotice>}
          {sent !== null && <Notice>{sent}</Notice>}
          <div className="crm-toolbar">
            <Button type="submit" variant="primary" size="sm" loading={pending === "preview"} disabled={pending !== null}>
              Preview plan
            </Button>
          </div>
        </form>
      )}
      {preview !== null && (
        <section aria-label="Import preview" className="grid gap-2">
          <div className="card px-3 py-2">
            <p className="text-label text-ink-200" data-preview-summary>
              Preview of {preview.rowCount} rows — {preview.summary.create} create ·{" "}
              {preview.summary.update} update · {preview.summary.needsRevision} need a revision ·{" "}
              {preview.summary.skip} already current · {preview.summary.error} refused
            </p>
            <p className="text-micro text-ink-500 mt-1">
              Nothing is written yet — this plan is bound to the exact CSV you pasted.
            </p>
          </div>
          <DataTable
            label="Planned rows"
            wrapClassName="crm-table-wrap"
            tableClassName="crm-table"
            rowKey={(row) => row.row}
            rowProps={(row) => ({ "data-plan-row": row.row })}
            rows={preview.rows}
            columns={[
              { key: "row", header: "Row", cellClassName: "num text-ink-500", cell: (row) => row.row },
              {
                key: "record",
                header: "Record",
                cellClassName: "num text-ink-300",
                cell: (row) => {
                  const view = row.profile !== null ? viewProfile(row.profile) : null;
                  return (
                    <>
                      {row.recordId}
                      {view !== null && <span className="text-ink-100"> — {view.displayName}</span>}
                    </>
                  );
                },
              },
              {
                key: "plan",
                header: "Plan",
                cell: (row) => (
                  <span className="chip" data-decision={row.decision}>
                    {row.decision === "needs_revision" ? "needs revision" : row.decision}
                  </span>
                ),
              },
              {
                key: "revision",
                header: "Revision",
                cell: (row) => {
                  const choice = choices.get(row.row) ?? csvPlanChoice(row);
                  return row.decision === "needs_revision" && choice === "apply" ? (
                    <input
                      id={`chat-csv-rev-${row.row}`}
                      className="field num"
                      style={{ width: "5rem" }}
                      inputMode="numeric"
                      maxLength={6}
                      autoComplete="off"
                      aria-label={`Expected revision for row ${row.row}`}
                      placeholder={row.currentRevision !== null ? `r${row.currentRevision}` : "revision"}
                      value={revisions.get(row.row) ?? ""}
                      onChange={(e) => setRevisions(new Map(revisions).set(row.row, e.target.value))}
                    />
                  ) : (
                    <span className="num text-ink-500">—</span>
                  );
                },
              },
              {
                key: "import",
                header: "Import",
                cell: (row) => {
                  const choice = choices.get(row.row) ?? csvPlanChoice(row);
                  return row.decision === "error" ? (
                    <span className="text-label text-ink-500">skipped — fix the row</span>
                  ) : (
                    <Select
                      id={`chat-csv-choice-${row.row}`}
                      size="sm"
                      value={choice}
                      onChange={(value) => {
                        if (value === "apply" || value === "skip") {
                          setChoices(new Map(choices).set(row.row, value));
                        }
                      }}
                      options={[
                        {
                          value: "apply",
                          label:
                            row.decision === "create"
                              ? "Create"
                              : row.decision === "needs_revision"
                                ? "Update with revision"
                                : "Update",
                        },
                        { value: "skip", label: "Skip" },
                      ]}
                      aria-label={`Import choice for row ${row.row}`}
                    />
                  );
                },
              },
            ]}
          />
          {error !== null && <ErrorNotice bare>{error}</ErrorNotice>}
          <div className="crm-toolbar">
            <Button
              variant="primary"
              size="sm"
              loading={pending === "confirm"}
              disabled={
                pending !== null ||
                applyCount === 0 ||
                !csvDecisionsReady(preview.rows, choices, revisions)
              }
              title={
                applyCount === 0
                  ? "No row is set to apply — the plan refused or skipped every row"
                  : csvDecisionsReady(preview.rows, choices, revisions)
                    ? "Confirm and hand the reviewed plan to the assistant's next turn"
                    : "Every row marked for update needs its expected revision"
              }
              onClick={confirmPlan}
            >
              Confirm &amp; hand to assistant ({applyCount})
            </Button>
            <Button size="sm" disabled={pending !== null} onClick={resetPlan}>
              Cancel
            </Button>
          </div>
        </section>
      )}
    </section>
  );
}
