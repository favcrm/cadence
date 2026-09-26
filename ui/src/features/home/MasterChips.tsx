import { useEffect, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import type { Agent, MasterState } from "../../lib/types";
import { masterModelsOptions, pickLanded, type MasterModelsView } from "./master";

/** Context chip's tooltip — the raw token counts when the provider sent them. */
function ctxTitle(m: MasterState | null | undefined): string {
  const c = m?.context;
  if (c?.tokens != null && c?.window != null) {
    return `${c.tokens.toLocaleString()} of ${c.window.toLocaleString()} tokens of context`;
  }
  return "Share of the context window in use";
}

const COST_CHIP: Record<string, string> = {
  Free: "bg-ok/15 text-ok",
  "Low cost": "bg-info/10 text-info",
  Paid: "bg-warn/10 text-warn",
};

/**
 * A chip's dropdown panel (CAD-574): absolutely positioned under the
 * chip, closed by the backdrop, Esc, or a pick. `open` toggles it.
 */
function ChipMenu({
  label,
  onClose,
  children,
}: {
  label: string;
  onClose: () => void;
  children: React.ReactNode;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <>
      <button
        type="button"
        aria-hidden
        tabIndex={-1}
        className="fixed inset-0 z-30 cursor-default"
        onClick={onClose}
      />
      <div className="chipmenu" role="menu" aria-label={label}>
        {children}
      </div>
    </>
  );
}

/**
 * The session chips (CAD-551), grown into dropdowns (CAD-574): the
 * model and effort chips open lists from `GET /api/master/models`
 * (CAD-575's route) — a refusal reads inline under the chip row instead
 * of silently dead-clicking. A pick relays `master_command` `model`/
 * `effort`; the chip then spins until `master_state` reflects the
 * choice, so a refused or still-applying swap never pretends to land.
 * Context use stays a plain read-out.
 */
export default function MasterChips({
  master,
  row,
  readOnly,
}: {
  master: MasterState | null | undefined;
  row?: Agent;
  readOnly: boolean;
}) {
  const model =
    master?.model_label ?? master?.model ?? row?.model ?? row?.model_reported ?? row?.model_configured;
  const effort = master?.effort ?? row?.effort ?? row?.effort_reported;
  const pct = master?.context?.percent;
  const live = master?.live === true;

  const [open, setOpen] = useState<"model" | "effort" | null>(null);
  const [view, setView] = useState<MasterModelsView | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** The pick still being applied — `kind`+`id`; clears when state agrees. */
  const [pending, setPending] = useState<{ kind: "model" | "effort"; id: string } | null>(null);

  // The picker's read: fetched on each open so the list never serves a
  // stale session; the last good payload stays rendered while it loads.
  const load = () => {
    setLoading(true);
    api
      .masterModels()
      .then((m) => {
        setView(masterModelsOptions(m));
        setError(null);
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setLoading(false));
  };
  const toggle = (which: "model" | "effort") => {
    if (open === which) {
      setOpen(null);
      return;
    }
    setOpen(which);
    load();
  };

  const pick = (kind: "model" | "effort", id: string) => {
    setOpen(null);
    setError(null);
    setPending({ kind, id });
    api
      .masterCommand(kind, id)
      .then(() => void resources.masterState.refresh())
      .catch((e: ApiError) => {
        setPending(null);
        setError(e.message ?? String(e));
      });
  };

  // The chip spins until the reported state agrees with the pick — a
  // provider that silently ignores the swap keeps the spin until the
  // next state lands it or the row is re-picked.
  useEffect(() => {
    if (!pending) return;
    const current =
      pending.kind === "model"
        ? (master?.model ?? row?.model_reported ?? row?.model_configured)
        : (master?.effort ?? row?.effort_reported ?? row?.effort);
    if (pickLanded(pending.kind, pending.id, current)) setPending(null);
  }, [pending, master, row]);
  // Never let a wedged provider hold the spin forever.
  useEffect(() => {
    if (!pending) return;
    const t = setTimeout(() => setPending(null), 15_000);
    return () => clearTimeout(t);
  }, [pending]);

  if (!model && !effort && pct == null) return null;
  const disabled = readOnly;
  return (
    <span className="flex items-center gap-1.5 min-w-0 flex-wrap">
      {model && (
        <span className="relative">
          <button
            type="button"
            key={`m:${model}`}
            className="chip chip-in bg-ink-800 text-ink-300 chip-btn"
            title={
              live ? "Reported by the live provider session — pick a model" : "Configured model — pick a model"
            }
            aria-expanded={open === "model"}
            disabled={disabled}
            onClick={() => toggle("model")}
          >
            {live && <span className="livedot" aria-hidden />}
            {pending?.kind === "model" ? `${pending.id}…` : model}
            <span aria-hidden> ▾</span>
          </button>
          {open === "model" && (
            <ChipMenu label="pick a model" onClose={() => setOpen(null)}>
              {loading && !view && <p className="px-2.5 py-1.5 text-label text-ink-500">Reading models…</p>}
              {error && !view && (
                <p className="px-2.5 py-1.5 text-label text-fail break-words" role="alert">
                  {error}
                </p>
              )}
              {view && view.models.length === 0 && (
                <p className="px-2.5 py-1.5 text-label text-ink-500">The provider lists no models.</p>
              )}
              {view?.models.map((m) => (
                <button
                  key={m.id}
                  type="button"
                  className="chipitem"
                  role="menuitem"
                  disabled={!!pending}
                  onClick={() => pick("model", m.id)}
                >
                  <span className="min-w-0 flex-1 truncate text-left">
                    {m.label}
                    {m.label !== m.id && <span className="num text-ink-500"> {m.id}</span>}
                  </span>
                  <span className={`chip shrink-0 ${COST_CHIP[m.cost] ?? "bg-ink-800 text-ink-400"}`}>
                    {m.cost}
                  </span>
                  {(view.model === m.id || model === m.label || model === m.id) && (
                    <span className="text-ok shrink-0" aria-label="current">
                      ✓
                    </span>
                  )}
                </button>
              ))}
            </ChipMenu>
          )}
        </span>
      )}
      {effort && (
        <span className="relative">
          <button
            type="button"
            key={`e:${effort}`}
            className="chip chip-in bg-ink-800 text-ink-300 chip-btn"
            title="Thinking effort — pick a level"
            aria-expanded={open === "effort"}
            disabled={disabled}
            onClick={() => toggle("effort")}
          >
            {pending?.kind === "effort" ? `effort ${pending.id}…` : `effort ${effort}`}
            <span aria-hidden> ▾</span>
          </button>
          {open === "effort" && (
            <ChipMenu label="pick a thinking level" onClose={() => setOpen(null)}>
              {loading && !view && <p className="px-2.5 py-1.5 text-label text-ink-500">Reading levels…</p>}
              {error && !view && (
                <p className="px-2.5 py-1.5 text-label text-fail break-words" role="alert">
                  {error}
                </p>
              )}
              {view && view.efforts.length === 0 && (
                <p className="px-2.5 py-1.5 text-label text-ink-500">The provider lists no levels.</p>
              )}
              {view?.efforts.map((e) => (
                <button
                  key={e}
                  type="button"
                  className="chipitem"
                  role="menuitem"
                  disabled={!!pending}
                  onClick={() => pick("effort", e)}
                >
                  <span className="min-w-0 flex-1 truncate text-left">{e}</span>
                  {(view.effort === e || effort === e) && (
                    <span className="text-ok shrink-0" aria-label="current">
                      ✓
                    </span>
                  )}
                </button>
              ))}
            </ChipMenu>
          )}
        </span>
      )}
      {pct != null && (
        <span
          key={`c:${Math.round(pct)}`}
          className={`chip chip-in ${
            pct >= 90 ? "bg-fail/15 text-fail" : pct >= 70 ? "bg-warn/10 text-warn" : "bg-ink-800 text-ink-300"
          }`}
          title={ctxTitle(master)}
        >
          {Math.round(pct)}% ctx
        </span>
      )}
      {pending && <span className="workdot live" aria-label="applying" />}
      {error && view && (
        <span className="text-micro text-fail break-words" role="alert">
          {error}
        </span>
      )}
    </span>
  );
}
