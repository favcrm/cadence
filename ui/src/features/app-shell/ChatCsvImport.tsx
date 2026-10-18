import { useEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
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
import { displayUiNotice, type UiNotice } from "../../lib/uiNotice";
import { friendlyError, viewProfile } from "./customerProfile";
import type { HostScope } from "./hostActions";

/**
 * The chat-bounded CSV import (CAD-1016): a pasted customer list is
 * previewed read-only into an exact create/update/skip/error plan; the
 * operator reviews the rows, resolves `needs_revision`, then Confirms.
 * The confirm stores the durable plan host-side (`csv-confirm` carries the
 * exact CSV bytes + the reviewed decisions, bound to the byte token +
 * request id + decisions digest) and returns the one-use `confirm_token`.
 * The scoped chat message then carries ONLY `{cadence_csv_import:
 * {request_id, confirm_token}}` — the agent's next turn resolves the
 * stored plan by that pair and redeems it via `csv-assistant-import`.
 * The CSV bytes and decisions never ride the text-only message, so no
 * size bound on the paste is needed beyond the daemon's own CSV cap, and
 * a plan can never be substituted for one the operator did not confirm.
 * No token is ever shown to copy, no CLI is run by the user, and the
 * CSV/decisions never reach a log or an error string. Cancel writes nothing.
 */

const CSV_HINT =
  "record_id,display_name,email,phone,tags,source,consent_email,consent_sms,expected_revision";

export default function ChatCsvImport({
  scope,
  canWrite,
  onSendIntent,
}: {
  scope: HostScope;
  canWrite: boolean;
  /** Sends the scoped chat message carrying only
   *  `{cadence_csv_import: {request_id, confirm_token}}` to the next
   *  agent turn — the durable plan lives host-side; the bytes/decisions
   *  never travel in the message. Returns null on success. */
  onSendIntent: (intent: {
    cadence_csv_import: { request_id: string; confirm_token: string };
  }) => Promise<string | null>;
}) {
  const t = useTranslate();
  const [csvText, setCsvText] = useState("");
  const [preview, setPreview] = useState<CsvPreview | null>(null);
  const [choices, setChoices] = useState<Map<number, CsvRowChoice>>(new Map());
  const [revisions, setRevisions] = useState<Map<number, string>>(new Map());
  const [requestId, setRequestId] = useState(newImportRequestId);
  const [pending, setPending] = useState<"preview" | "confirm" | null>(null);
  const [error, setError] = useState<UiNotice | null>(null);
  // UiNotice keeps notice state as the authored English key/slots so the
  // stored message still renders in the active locale after a switch — a
  // pre-translated string would freeze the completion-time locale.
  const [sent, setSent] = useState<UiNotice | null>(null);
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
    const digest = csvDecisionsDigest(decisions);
    setPending("confirm");
    setError(null);
    try {
      // The confirm mints the durable plan host-side: csv_text + the
      // reviewed decisions + the digest all travel to the store here —
      // the bytes never ride the text-only chat.
      const receipt = await csvActions.confirm(
        scope,
        csvText,
        preview.previewToken,
        requestId,
        digest,
        wireDecisions.length > 0 ? wireDecisions : undefined,
      );
      if (!live.current.mounted || live.current.scope !== scopeKey) return;
      // Hand off only the confirm receipt to the agent's scoped turn —
      // the message carries just request_id + confirm_token; the agent
      // resolves the stored plan by that pair.
      const sendError = await onSendIntent({
        cadence_csv_import: { request_id: requestId, confirm_token: receipt.confirmToken },
      });
      if (!live.current.mounted || live.current.scope !== scopeKey) return;
      if (sendError === null) {
        // Clear the plan first, then set the status — resetPlan() clears
        // `sent`, so the message is written after the reset, not before.
        resetPlan();
        setSent("Confirmed — the import went to the assistant. It applies the reviewed rows; this board shows the outcome.");
      } else {
        // The confirm minted but the hand-off send failed — say so
        // honestly; the operator retries the same request id, which the
        // daemon replays/redeems idempotently.
        setError({
          key: "Confirmed, but the assistant turn did not start: {error}. Retry the same preview to hand it off again.",
          slots: { error: sendError },
        });
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
          ? t("Pick a customer context before importing through chat.")
          : t("Read-only view. Customer imports through the assistant are disabled here.")}
      </Notice>
    );
  }

  const applyCount = preview === null ? 0 : csvApplyCount(preview.rows, choices);

  return (
    <section aria-label={t("Import customers via chat")} className="grid gap-2" data-chat-import>
      <p className="text-label text-ink-400">
        {t("Paste a small customer CSV — review each row, then Confirm to import it with the assistant. Nothing imports until you Confirm.")}
      </p>
      {preview === null && (
        <form
          className="grid gap-2"
          aria-label={t("CSV source")}
          onSubmit={(e) => {
            e.preventDefault();
            runPreview();
          }}
        >
          <Field label={t("CSV text — header plus one row per customer")} id="chat-csv-text" className="crm-field">
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
          {error !== null && <ErrorNotice bare>{displayUiNotice(error, t)}</ErrorNotice>}
          {sent !== null && <Notice>{displayUiNotice(sent, t)}</Notice>}
          <div className="crm-toolbar">
            <Button type="submit" variant="primary" size="sm" loading={pending === "preview"} disabled={pending !== null}>
              {t("Preview plan")}
            </Button>
          </div>
        </form>
      )}
      {preview !== null && (
        <section aria-label={t("Import preview")} className="grid gap-2">
          <div className="card px-3 py-2">
            <p className="text-label text-ink-200" data-preview-summary>
              {t("Preview of {count} rows — {create} create · {update} update · {revision} need a revision · {skip} already current · {error} refused")
                .replace("{count}", String(preview.rowCount))
                .replace("{create}", String(preview.summary.create))
                .replace("{update}", String(preview.summary.update))
                .replace("{revision}", String(preview.summary.needsRevision))
                .replace("{skip}", String(preview.summary.skip))
                .replace("{error}", String(preview.summary.error))}
            </p>
            <p className="text-micro text-ink-500 mt-1">
              {t("Nothing is written yet — this plan is bound to the exact CSV you pasted.")}
            </p>
          </div>
          <DataTable
            label={t("Planned rows")}
            wrapClassName="crm-table-wrap"
            tableClassName="crm-table"
            rowKey={(row) => row.row}
            rowProps={(row) => ({ "data-plan-row": row.row })}
            rows={preview.rows}
            columns={[
              { key: "row", header: t("Row"), cellClassName: "num text-ink-500", cell: (row) => row.row },
              {
                key: "record",
                header: t("Record"),
                cellClassName: "num text-ink-300",
                cell: (row) => {
                  const view = row.profile !== null ? viewProfile(row.profile) : null;
                  return (
                    <>
                      {row.recordId !== "" ? row.recordId : <span className="text-ink-500">{t("no record id")}</span>}
                      {view !== null && <span className="text-ink-100"> — {view.displayName}</span>}
                    </>
                  );
                },
              },
              {
                key: "plan",
                header: t("Plan"),
                cell: (row) => (
                  <span className="chip" data-decision={row.decision}>
                    {t(row.decision === "needs_revision" ? "Needs revision" : row.decision === "create" ? "Create" : row.decision === "update" ? "Update" : row.decision === "error" ? "Error" : row.decision === "skip" ? "Skip" : row.decision).toLowerCase()}
                  </span>
                ),
              },
              {
                key: "revision",
                header: t("Revision"),
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
                      aria-label={t("Expected revision for row {row}").replace("{row}", String(row.row))}
                      placeholder={row.currentRevision !== null ? `r${row.currentRevision}` : "revision"}
                      value={revisions.get(row.row) ?? ""}
                      disabled={pending !== null}
                      onChange={(e) => setRevisions(new Map(revisions).set(row.row, e.target.value))}
                    />
                  ) : (
                    <span className="num text-ink-500">—</span>
                  );
                },
              },
              {
                key: "import",
                header: t("import").replace(/^./, (char) => char.toUpperCase()),
                cell: (row) => {
                  const choice = choices.get(row.row) ?? csvPlanChoice(row);
                  return row.decision === "error" ? (
                    <span className="text-label text-ink-500">
                      {t("skipped — ")}{row.errors.length > 0 ? row.errors.join(" · ") : (row.reason ?? t("fix the row"))}
                    </span>
                  ) : (
                    <Select
                      id={`chat-csv-choice-${row.row}`}
                      size="sm"
                      value={choice}
                      disabled={pending !== null}
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
                              ? t("Create")
                              : row.decision === "needs_revision"
                                ? t("Update with revision")
                                : t("Update"),
                        },
                        { value: "skip", label: t("Skip") },
                      ]}
                      aria-label={t("Import choice for row {row}").replace("{row}", String(row.row))}
                    />
                  );
                },
              },
            ]}
          />
          {error !== null && <ErrorNotice bare>{displayUiNotice(error, t)}</ErrorNotice>}
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
                  ? t("No row is set to apply — the plan refused or skipped every row")
                  : csvDecisionsReady(preview.rows, choices, revisions)
                    ? t("Confirm this plan — the assistant imports the reviewed rows")
                    : t("Every row marked for update needs its expected revision")
              }
              onClick={confirmPlan}
            >
              {t("Confirm")} {t("import")} ({applyCount})
            </Button>
            <Button size="sm" disabled={pending !== null} onClick={resetPlan}>
              {t("Cancel")}
            </Button>
          </div>
        </section>
      )}
    </section>
  );
}
