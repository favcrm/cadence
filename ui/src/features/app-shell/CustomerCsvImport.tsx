import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import type { Viewer } from "../projects/work";
import {
  CSV_MAX_BYTES,
  buildCsvDecisions,
  csvActions,
  csvDecisionsReady,
  csvPlanChoice,
  newImportRequestId,
  type CsvImportReceipt,
  type CsvPreview,
  type CsvRowChoice,
} from "./csvClient";
import { friendlyError, viewProfile } from "./customerProfile";
import type { HostScope } from "./hostActions";

/**
 * The CSV import page inside CRM → Customers (CAD-865): paste or pick a
 * bounded CSV, preview the server's per-row plan without mutating
 * anything, resolve `needs_revision`/`error` rows with explicit operator
 * decisions, then commit through the daemon's token-bound import verb.
 *
 * The preview token binds the exact bytes — editing the text afterwards
 * drops the plan and demands a fresh preview, so an import can never
 * commit bytes the operator did not see planned. Consent is never
 * inferred: rows with no consent cells arrive as `unknown` in the plan.
 * The page's state lives only in this component — nothing enters the
 * route URL, history or session storage.
 */

/** The example header the placeholder documents — the daemon's column
 *  grammar, order-free but displayed in its documented order. */
const CSV_HINT =
  "record_id,display_name,email,phone,tags,source,consent_email,consent_sms,expected_revision";



const OUTCOME_LABEL: Record<string, string> = {
  created: "created",
  updated: "updated",
  skipped: "skipped",
  failed: "refused",
};

export default function CustomerCsvImport({
  scope,
  viewer,
  onDone,
  onCancel,
}: {
  scope: HostScope;
  viewer: Viewer;
  /** The import committed — the parent refreshes its list when the
   *  operator chooses to return; this page keeps its receipt visible
   *  until then. */
  onDone: () => void;
  onCancel: () => void;
}) {
  const canWrite = viewer.operator && !viewer.readOnly;
  const headRef = useRef<HTMLHeadingElement | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);
  const [csvText, setCsvText] = useState("");
  const [preview, setPreview] = useState<CsvPreview | null>(null);
  const [receipt, setReceipt] = useState<CsvImportReceipt | null>(null);
  const [choices, setChoices] = useState<Map<number, CsvRowChoice>>(new Map());
  const [revisions, setRevisions] = useState<Map<number, string>>(new Map());
  const [pending, setPending] = useState<"preview" | "import" | null>(null);
  const [error, setError] = useState<string | null>(null);
  // A monotonic pick sequence — the newest file pick or manual paste
  // wins over every in-flight read.
  // The request id is minted once per preview and rides every commit of
  // that preview — a retry after an uncertain answer replays the stored
  // receipt rather than double-applying.
  const [requestId, setRequestId] = useState(newImportRequestId);
  // Everything async lands through `live`: a response whose scope or
  // mount no longer matches is discarded, never applied — a cancelled
  // page and a remounted scope can never see a stale write land.
  const live = useRef<{ mounted: boolean; scope: string }>({ mounted: true, scope: "" });
  live.current.scope = `${scope.installId}:${scope.contextId}`;
  useEffect(() => {
    headRef.current?.focus();
    const entry = live.current;
    entry.mounted = true;
    return () => {
      entry.mounted = false;
    };
  }, []);

  // The selected file's byte size is checked before the read — a file
  // over the daemon's 256 KiB bound is refused without ever being read.
  // A monotonically increasing pick id makes the newest pick win: a slow
  // read landing after a newer pick (or a manual paste) can never
  // overwrite it.
  // Any new file selection — including one we then refuse — discards
  // the prior plan before it can be committed under a file the
  // operator no longer means to import. `resetPlan` runs before the
  // size check, the read and the error path alike.
  const pickSeq = useRef(0);
  const resetPlan = () => {
    setCsvText("");
    setPreview(null);
    setReceipt(null);
    setChoices(new Map());
    setRevisions(new Map());
  };
  const pickFile = (file: File | undefined) => {
    if (!file) return;
    const seq = ++pickSeq.current;
    resetPlan();
    if (file.size > CSV_MAX_BYTES) {
      // Clear the input so re-picking the same file re-fires onChange.
      if (fileRef.current) fileRef.current.value = "";
      setError(`The selected file exceeds the 256 KiB bound (${Math.round(file.size / 1024)} KiB).`);
      return;
    }
    setError(null);
    const scopeKey = live.current.scope;
    file
      .text()
      .then((text) => {
        // A read that lands after a scope change, unmount or a newer
        // pick is dropped — never overwrites newer bytes.
        if (!live.current.mounted || live.current.scope !== scopeKey || seq !== pickSeq.current) {
          return;
        }
        setCsvText(text);
        setPreview(null);
        setReceipt(null);
        setChoices(new Map());
        setRevisions(new Map());
      })
      .catch(() => {
        if (live.current.mounted && live.current.scope === scopeKey && seq === pickSeq.current) {
          setError("The selected file could not be read.");
        }
      });
  };

  const editText = (text: string) => {
    // A manual paste is a newer edit than any in-flight file read and
    // invalidates any plan bound to the earlier bytes.
    pickSeq.current += 1;
    setCsvText(text);
    if (preview !== null || receipt !== null) {
      setPreview(null);
      setReceipt(null);
      setChoices(new Map());
      setRevisions(new Map());
    }
  };

  const runPreview = () => {
    setPending("preview");
    setError(null);
    setReceipt(null);
    const scopeKey = live.current.scope;
    const text = csvText;
    void csvActions
      .preview(scope, text)
      .then((plan) => {
        if (!live.current.mounted || live.current.scope !== scopeKey) return;
        setPreview(plan);
        setRequestId(newImportRequestId());
        setChoices(new Map(plan.rows.map((row) => [row.row, csvPlanChoice(row)])));
        setRevisions(
          new Map(
            plan.rows
              .filter((row) => row.decision === "needs_revision")
              .map((row) => [
                row.row,
                String(row.currentRevision ?? row.expectedRevision ?? ""),
              ]),
          ),
        );
      })
      .catch((e: unknown) => {
        if (live.current.mounted && live.current.scope === scopeKey) {
          setError(friendlyError(e));
        }
      })
      .finally(() => {
        if (live.current.mounted && live.current.scope === scopeKey) setPending(null);
      });
  };

  const runImport = () => {
    if (preview === null) return;
    setPending("import");
    setError(null);
    const scopeKey = live.current.scope;
    const decisions = buildCsvDecisions(preview.rows, choices, revisions);
    void csvActions
      .import(scope, csvText, preview.previewToken, requestId, decisions)
      .then((result) => {
        if (!live.current.mounted || live.current.scope !== scopeKey) return;
        // The receipt stays mounted — applied/skipped/refused counts
        // and per-row reasons are the operator's durable answer.
        setReceipt(result);
      })
      .catch((e: unknown) => {
        if (live.current.mounted && live.current.scope === scopeKey) {
          setError(friendlyError(e));
        }
      })
      .finally(() => {
        if (live.current.mounted && live.current.scope === scopeKey) setPending(null);
      });
  };

  if (!canWrite) {
    return (
      <section aria-label="Import customers">
        <h3 ref={headRef} className="text-cardtitle font-medium text-ink-100" tabIndex={-1} data-outlet-heading>
          Import customers
        </h3>
        <p className="card px-4 py-3 mt-2 text-label text-ink-400" data-state="read-only">
          {viewer.operator
            ? "Read-only view. Customer imports are disabled on this board."
            : "Sign in as the operator to import customer records."}
        </p>
      </section>
    );
  }
  if (scope.contextId === "") {
    return (
      <section aria-label="Import customers">
        <h3 ref={headRef} className="text-cardtitle font-medium text-ink-100" tabIndex={-1} data-outlet-heading>
          Import customers
        </h3>
        <p className="card px-4 py-3 mt-2 text-label text-ink-400">
          Pick an App context above before importing customers.
        </p>
      </section>
    );
  }

  return (
    <section aria-label="Import customers" className="grid gap-3">
      <div>
        <h3 ref={headRef} className="text-cardtitle font-medium text-ink-100" tabIndex={-1} data-outlet-heading>
          Import customers
        </h3>
        <p className="text-label text-ink-400 mt-1">
          <button type="button" className="lnk" onClick={onCancel}>
            ← Customers
          </button>{" "}
          · Context {scope.contextId} — the preview writes nothing; the import commits the rows
          you approve below.
        </p>
      </div>

      {receipt === null && (
        <form
          className="card px-4 py-4 grid gap-3"
          aria-label="CSV source"
          onSubmit={(e) => {
            e.preventDefault();
            try {
              if (csvText.trim() === "") {
                throw new ApiError("the CSV is empty — pick a file or paste rows first", 400);
              }
              runPreview();
            } catch (e: unknown) {
              setError(friendlyError(e));
            }
          }}
        >
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="csv-file">
              CSV file (up to 256 KiB, 500 rows)
            </label>
            <input
              id="csv-file"
              ref={fileRef}
              type="file"
              accept=".csv,text/csv,text/plain"
              disabled={pending !== null}
              onChange={(e) => pickFile(e.target.files?.[0])}
            />
          </div>
          <div className="crm-field">
            <label className="text-label text-ink-300" htmlFor="csv-text">
              CSV text — header plus one row per customer
            </label>
            <textarea
              id="csv-text"
              className="field"
              rows={8}
              value={csvText}
              onChange={(e) => editText(e.target.value)}
              disabled={pending !== null}
              spellCheck={false}
              autoComplete="off"
              placeholder={CSV_HINT}
            />
          </div>
          <p className="text-micro text-ink-500">
            Columns: record_id (required), display_name (required), email, phone, tags
            (semicolon-separated), source, consent_email, consent_sms, expected_revision. Empty
            consent cells import as unknown — consent is never inferred.
          </p>
          {error !== null && (
            <p className="text-label text-fail" role="alert">
              {error}
            </p>
          )}
          <div>
            <Button
              type="submit"
              variant="primary"
              loading={pending === "preview"}
              disabled={pending !== null}
            >
              Preview plan
            </Button>
          </div>
        </form>
      )}

      {preview !== null && receipt === null && (
        <section aria-label="Import preview" className="grid gap-3">
          <div className="card px-4 py-3">
            <p className="text-label text-ink-200" data-preview-summary>
              Preview of {preview.rowCount} rows — {preview.summary.create} create ·{" "}
              {preview.summary.update} update · {preview.summary.needsRevision} need a revision ·{" "}
              {preview.summary.skip} already current · {preview.summary.error} refused
            </p>
            <p className="text-micro text-ink-500 mt-1">
              Nothing is written yet. Preview token {preview.previewToken.slice(0, 24)}… binds these
              exact bytes — changing the text above requires a new preview.
            </p>
          </div>
          <div
            className="crm-table-wrap"
            tabIndex={0}
            role="region"
            aria-label="Planned rows — scroll horizontally to reach every column"
          >
            <table className="crm-table">
              <thead>
                <tr>
                  <th scope="col">Row</th>
                  <th scope="col">Record</th>
                  <th scope="col">Plan</th>
                  <th scope="col">Detail</th>
                  <th scope="col">Revision</th>
                  <th scope="col">Import</th>
                </tr>
              </thead>
              <tbody>
                {preview.rows.map((row) => {
                  const choice = choices.get(row.row) ?? csvPlanChoice(row);
                  const view = row.profile !== null ? viewProfile(row.profile) : null;
                  return (
                    <tr key={row.row} data-plan-row={row.row}>
                      <td className="num text-ink-500">{row.row}</td>
                      <td className="num text-ink-300">
                        {row.recordId}
                        {view !== null && (
                          <span className="text-ink-100"> — {view.displayName}</span>
                        )}
                      </td>
                      <td>
                        <span className="chip" data-decision={row.decision}>
                          {row.decision === "needs_revision" ? "needs revision" : row.decision}
                        </span>
                      </td>
                      <td className="text-label text-ink-300">
                        {row.errors.length > 0
                          ? row.errors.join(" · ")
                          : (row.reason ??
                            (row.duplicateOf !== null ? `duplicate of ${row.duplicateOf}` : "—"))}
                      </td>
                      <td>
                        {row.decision === "needs_revision" && choice === "apply" ? (
                          <input
                            id={`csv-revision-${row.row}`}
                            className="field num"
                            style={{ width: "5rem" }}
                            inputMode="numeric"
                            maxLength={6}
                            autoComplete="off"
                            aria-label={`Expected revision for row ${row.row}`}
                            value={revisions.get(row.row) ?? ""}
                            onChange={(e) =>
                              setRevisions(new Map(revisions).set(row.row, e.target.value))
                            }
                          />
                        ) : row.decision === "update" ? (
                          <span className="num text-ink-300">
                            r{row.expectedRevision ?? "—"} → r
                            {row.currentRevision !== null ? row.currentRevision + 1 : "?"}
                          </span>
                        ) : (
                          <span className="num text-ink-500">—</span>
                        )}
                      </td>
                      <td>
                        {row.decision === "error" ? (
                          <span className="text-label text-ink-500">skipped — fix the row</span>
                        ) : (
                          <Select
                            id={`csv-choice-${row.row}`}
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
                                      : row.decision === "update"
                                        ? "Update"
                                        : "Apply",
                              },
                              { value: "skip", label: "Skip" },
                            ]}
                            aria-label={`Import choice for row ${row.row}`}
                          />
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
          {error !== null && (
            <p className="text-label text-fail" role="alert">
              {error}
            </p>
          )}
          <div className="crm-toolbar">
            <Button
              variant="primary"
              loading={pending === "import"}
              disabled={
                pending !== null ||
                preview.summary.error === preview.rowCount ||
                !csvDecisionsReady(preview.rows, choices, revisions)
              }
              title={
                preview.summary.error === preview.rowCount
                  ? "No row is importable — every row refused in the preview"
                  : csvDecisionsReady(preview.rows, choices, revisions)
                    ? "Commit the approved rows under this exact preview"
                    : "Every row marked for update needs its expected revision"
              }
              onClick={runImport}
            >
              Import {preview.rowCount - preview.summary.error} approved rows
            </Button>
            <Button
              size="sm"
              disabled={pending !== null}
              onClick={() => {
                setPreview(null);
                setChoices(new Map());
                setRevisions(new Map());
                setError(null);
              }}
            >
              Discard preview
            </Button>
          </div>
        </section>
      )}

      {receipt !== null && (
        <section aria-label="Import receipt" className="grid gap-3">
          <div className="card px-4 py-3" data-import-receipt>
            <p className="text-label text-ink-200">
              {receipt.replayed ? "This import already ran — replaying its receipt. " : ""}
              {receipt.summary.applied} applied · {receipt.summary.skipped} skipped ·{" "}
              {receipt.summary.failed} refused
            </p>
            <p className="text-micro text-ink-500 mt-1">
              Request {receipt.requestId} — retrying it replays this receipt, never double-applies.
            </p>
          </div>
          {receipt.rows.some((row) => row.outcome !== "created" && row.outcome !== "updated") && (
            <div
              className="crm-table-wrap"
              tabIndex={0}
              role="region"
              aria-label="Rows that did not apply — scroll horizontally to reach every column"
            >
              <table className="crm-table">
                <thead>
                  <tr>
                    <th scope="col">Row</th>
                    <th scope="col">Record</th>
                    <th scope="col">Outcome</th>
                    <th scope="col">Reason</th>
                  </tr>
                </thead>
                <tbody>
                  {receipt.rows
                    .filter((row) => row.outcome !== "created" && row.outcome !== "updated")
                    .map((row) => (
                      <tr key={row.row} data-outcome-row={row.row}>
                        <td className="num text-ink-500">{row.row}</td>
                        <td className="num text-ink-300">{row.recordId}</td>
                        <td>
                          <span className="chip" data-outcome={row.outcome}>
                            {OUTCOME_LABEL[row.outcome] ?? row.outcome}
                          </span>
                        </td>
                        <td className="text-label text-ink-300">{row.reason ?? "—"}</td>
                      </tr>
                    ))}
                </tbody>
              </table>
            </div>
          )}
          <div className="crm-toolbar">
            <Button size="sm" variant="primary" onClick={onDone}>
              Return to customers
            </Button>
            <Button
              size="sm"
              onClick={() => {
                setCsvText("");
                setPreview(null);
                setReceipt(null);
                setChoices(new Map());
                setRevisions(new Map());
                setError(null);
                if (fileRef.current) fileRef.current.value = "";
              }}
            >
              Import another file
            </Button>
          </div>
        </section>
      )}
    </section>
  );
}
