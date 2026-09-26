import { useEffect, useMemo, useState } from "react";
import { api, ApiError, type WriteResp } from "../../lib/api";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import {
  briefPreview,
  EFFORTS,
  KICKOFF_CHOICES,
  kickoffRequest,
  type KickoffChoice,
} from "./model";

interface Props {
  id: string;
  title: string;
  body: string;
  groups: string[];
  blocked: string | null;
  onClose: () => void;
  onWrite: (resp: WriteResp, verb: string) => void;
  onDone: (text: string) => void;
  planHref: string;
}

export default function KickoffDialog({ id, title, body, groups, blocked, onClose, onWrite, onDone, planHref }: Props) {
  const [step, setStep] = useState<"edit" | "confirm">("edit");
  const [provider, setProvider] = useState(KICKOFF_CHOICES[0].id);
  const [model, setModel] = useState(KICKOFF_CHOICES[0].models[0].id);
  const [effort, setEffort] = useState(KICKOFF_CHOICES[0].effort);
  const [group, setGroup] = useState(groups[0] ?? "master");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  useEffect(() => {
    let live = true;
    api
      .kickoffOptions(id)
      .then((opts) => {
        if (!live) return;
        const d = opts.defaults;
        if (d.provider && KICKOFF_CHOICES.some((c) => c.id === d.provider)) setProvider(d.provider);
        if (d.model) setModel(d.model);
        if (d.effort) setEffort(d.effort);
        if (d.group) setGroup(d.group);
      })
      .catch(() => {
        // Options are best-effort. The dialog keeps the local catalog.
      });
    return () => {
      live = false;
    };
  }, [id]);

  const choice: KickoffChoice = KICKOFF_CHOICES.find((c) => c.id === provider) ?? KICKOFF_CHOICES[0];
  const models = useMemo(() => {
    if (choice.models.some((m) => m.id === model)) return choice.models;
    return [{ id: model, cost: "unknown" }, ...choice.models];
  }, [choice, model]);
  const cost = models.find((m) => m.id === model)?.cost ?? choice.models[0].cost;
  const paid = cost === "Paid";
  const preview = briefPreview(id, title, body, note);
  const who = `${choice.label} · ${model} · ${effort}`;

  const pickProvider = (next: KickoffChoice) => {
    setProvider(next.id);
    setModel(next.models[0].id);
    setEffort(next.effort);
  };

  const send = () => {
    if (blocked || busy || !group.trim()) return;
    const req = kickoffRequest(id, { group, provider, model, effort, note });
    setBusy(true);
    setError(null);
    api
      .kickoff(id, req.body)
      .then((resp) => {
        if (resp && typeof resp === "object" && "issue" in resp && "card" in resp) {
          onWrite(resp as WriteResp, `${id} kick off`);
        } else {
          onDone(`${id} kick off sent`);
        }
        onClose();
      })
      .catch((e: unknown) => {
        const err = e as ApiError;
        if (err.status === 404) {
          setError("This board has no kickoff route. Nothing was dispatched.");
        } else {
          setError(err.message || "Kick off failed.");
        }
      })
      .finally(() => setBusy(false));
  };

  return (
    <div className="fixed inset-0 z-40 bg-scrim grid place-items-center p-4" onClick={onClose}>
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="kick-title"
        className="card w-full max-w-[680px] max-h-[min(860px,calc(100vh-48px))] flex flex-col bg-ink-875"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="flex items-baseline justify-between gap-3 px-4 py-3 border-b border-ink-700">
          <h2 id="kick-title" className="text-cardtitle font-semibold text-ink-100">
            {step === "edit" ? "Kick off" : "Confirm kick off"}
          </h2>
          <span className="kicker">{step === "edit" ? id : "operator"}</span>
        </header>
        <div className="overflow-auto px-4 py-4 grid gap-3">
          {blocked && <p className="text-secondary text-fail m-0" role="alert">{blocked}</p>}
          {step === "edit" ? (
            <>
              <p className="text-secondary text-ink-400 m-0">
                One operator-gated click runs issue start, then dispatch.
              </p>
              <div className="grid gap-1.5">
                <span className="slabel">Provider</span>
                <div className="flex gap-1" role="group" aria-label="Provider">
                  {KICKOFF_CHOICES.map((c) => (
                    <Button
                      key={c.id}
                      className="flex-1"
                      variant={c.id === provider ? "primary" : "secondary"}
                      aria-label={c.label}
                      onClick={() => pickProvider(c)}
                    >
                      {c.label}
                    </Button>
                  ))}
                </div>
              </div>
              <div className="grid sm:grid-cols-2 gap-2.5">
                <label className="grid gap-1.5">
                  <span className="slabel">Model</span>
                  <Select
                    full
                    value={model}
                    aria-label="Model"
                    options={models.map((m) => ({ value: m.id, label: m.id, badge: m.cost }))}
                    onChange={setModel}
                  />
                </label>
                <label className="grid gap-1.5">
                  <span className="slabel">Effort</span>
                  <Select
                    full
                    value={effort}
                    aria-label="Effort"
                    options={(EFFORTS.includes(effort) ? EFFORTS : [effort, ...EFFORTS]).map((e) => ({ value: e, label: e }))}
                    onChange={setEffort}
                  />
                </label>
              </div>
              <div className="flex items-center justify-between gap-2">
                <span className="slabel">Cost</span>
                <span className={`chip ${paid ? "bg-warn/15 text-warn" : "bg-info/15 text-info"}`}>{cost}</span>
              </div>
              <label className="grid gap-1.5">
                <span className="slabel">PM group</span>
                <input className="field w-full" list="kickoff-groups" value={group} onChange={(e) => setGroup(e.target.value)} aria-label="PM group" />
                <datalist id="kickoff-groups">
                  {groups.map((g) => (
                    <option key={g} value={g} />
                  ))}
                </datalist>
              </label>
              <label className="grid gap-1.5">
                <span className="slabel">Extra note</span>
                <textarea className="field w-full !h-auto py-2" rows={2} value={note} onChange={(e) => setNote(e.target.value)} placeholder="Optional note, appended on top of the issue body." />
              </label>
              <div className="grid gap-1.5">
                <span className="slabel">Brief preview</span>
                <pre className="m-0 max-h-40 overflow-auto px-3 py-2.5 rounded-lg border border-ink-700 bg-ink-900 text-secondary text-ink-300 whitespace-pre-wrap">{preview}</pre>
              </div>
              <Link className="lnk text-label" href={planHref}>Plan with master instead</Link>
            </>
          ) : (
            <>
              <div className="flex items-center gap-2 px-2.5 py-2 rounded border border-ink-700 bg-ink-900 text-label text-ink-300">
                <span className="chip bg-ink-800 text-ink-300">operator</span>
                This click runs issue start, then dispatch.
              </div>
              <dl className="grid grid-cols-[92px_minmax(0,1fr)] gap-x-2 gap-y-1.5">
                <dt className="slabel">Issue</dt><dd className="num text-secondary text-ink-200">{id}</dd>
                <dt className="slabel">Runs</dt><dd className="text-secondary text-ink-200">issue start, then dispatch</dd>
                <dt className="slabel">Worker</dt><dd className="text-secondary text-ink-200">{who}</dd>
                <dt className="slabel">Cost</dt><dd><span className={`chip ${paid ? "bg-warn/15 text-warn" : "bg-info/15 text-info"}`}>{cost}</span></dd>
                <dt className="slabel">PM</dt><dd className="text-secondary text-ink-200">{group}</dd>
              </dl>
              {paid && (
                <div className="border border-ink-700 border-l-[3px] border-l-warn rounded-lg px-3.5 py-3 bg-warn/10">
                  <strong className="text-ink-100">Paid provider</strong>
                  <p className="text-secondary text-ink-300 mt-1 m-0">OpenRouter spends money. Confirm only if that cost is intended.</p>
                </div>
              )}
              <div className="grid gap-1.5">
                <span className="slabel">Brief that would be sent</span>
                <pre className="m-0 max-h-40 overflow-auto px-3 py-2.5 rounded-lg border border-ink-700 bg-ink-900 text-secondary text-ink-300 whitespace-pre-wrap">{preview}</pre>
              </div>
              {error && <p className="text-secondary text-fail m-0" role="alert">{error}</p>}
            </>
          )}
        </div>
        <footer className="flex justify-end gap-2 px-4 py-3 border-t border-ink-700">
          {step === "edit" ? (
            <>
              <Button onClick={onClose}>Cancel</Button>
              <Button variant="primary" disabled={!!blocked || busy || !group.trim()} onClick={() => { if (!blocked && !busy && group.trim()) setStep("confirm"); }}>Continue</Button>
            </>
          ) : (
            <>
              <Button onClick={() => setStep("edit")}>Back</Button>
              <Button variant="primary" loading={busy} disabled={!!blocked || !group.trim()} onClick={send}>
                Kick off
              </Button>
            </>
          )}
        </footer>
      </div>
    </div>
  );
}
