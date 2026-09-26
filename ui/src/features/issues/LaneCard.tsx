import { useContext, useState } from "react";
import { WriteGate } from "../auth/WriteGate";
import { UNFENCE_CHOICES } from "../home/needs";
import { api, ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import {
  branchTitle,
  composerBlocked,
  costLabel,
  effortChoices,
  modelChoices,
  reassignBody,
  unfenceReady,
  type LaneProvider,
  type LaneView,
} from "./lane";

export type { LanePayload, LanePr, LaneProvider, LaneView } from "./lane";

const BADGE: Record<string, string> = {
  busy: "bg-info/10 text-ink",
  idle: "bg-ok/15 text-ink",
  fenced: "bg-warn/10 text-ink",
  quota: "bg-fail/10 text-fail",
  "rate-limited": "bg-fail/10 text-fail",
  shipped: "bg-ok/15 text-ink",
};

function badgeClass(state: string): string {
  return BADGE[state] ?? "bg-info/10 text-ink";
}

function activityLine(activity: LaneView["activity"]): string {
  if (!activity) return "—";
  const when = new Date(activity.at * 1000);
  const stamp = Number.isNaN(when.getTime()) ? "" : when.toLocaleString();
  return stamp ? `${activity.state} · ${stamp}` : activity.state;
}

/** The issue page's lane card. CAD-607 slots this; it does not own a route. */
export function LaneCard({
  issue,
  lane,
  providers,
  onCompose,
  onChanged,
}: {
  issue: string;
  lane: LaneView | null;
  providers: LaneProvider[];
  onCompose?: (mode: "ask" | "instruct") => void;
  onChanged?: () => void;
}) {
  const writeBlock = useContext(WriteGate);
  const [dialog, setDialog] = useState<"interrupt" | "unfence" | "reassign" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (!lane) {
    return (
      <section className="card" data-testid="lane-card">
        <h2 className="text-ink">Lane</h2>
        <p data-testid="lane-empty">No lane yet</p>
      </section>
    );
  }

  const blocked = composerBlocked(lane.state) || writeBlock != null;
  const run = async (work: () => Promise<unknown>) => {
    setError(null);
    setBusy(true);
    try {
      await work();
      setDialog(null);
      onChanged?.();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "the lane action failed");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="card" data-testid="lane-card">
      <header className="flex items-center gap-2">
        <h2 className="text-ink">Lane</h2>
        <span className={`chip ${badgeClass(lane.state)}`} data-testid="lane-state">
          {lane.state}
        </span>
      </header>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
        <dt>Agent</dt>
        <dd className="num" data-testid="lane-agent">{lane.agent}</dd>
        <dt>Provider</dt>
        <dd data-testid="lane-provider">{lane.provider}</dd>
        <dt>Model</dt>
        <dd className="num" data-testid="lane-model">{lane.model ?? "—"}</dd>
        <dt>Cost</dt>
        <dd data-testid="lane-cost">{lane.cost ?? "—"}</dd>
        <dt>Branch</dt>
        <dd className="num truncate" title={branchTitle(lane.branch)} data-testid="lane-branch">
          {lane.branch ?? "—"}
        </dd>
        <dt>PR</dt>
        <dd data-testid="lane-pr">
          {lane.pr?.url ? (
            <a href={lane.pr.url}>{lane.pr.label || lane.pr.url}</a>
          ) : (
            "—"
          )}
        </dd>
        <dt>Activity</dt>
        <dd data-testid="lane-activity">{activityLine(lane.activity)}</dd>
      </dl>
      {lane.state === "quota" && (
        <p className="text-fail">Reassign is the suggested recovery.</p>
      )}
      {lane.state === "shipped" && <p>The lane is closed.</p>}
      {error && <p className="text-fail" data-testid="lane-error">{error}</p>}
      <div className="flex flex-wrap gap-2">
        <span data-testid="lane-ask" className="inline-flex">
          <Button size="sm" disabled={blocked || busy} onClick={() => onCompose?.("ask")}>
            Ask for status
          </Button>
        </span>
        <span data-testid="lane-instruct" className="inline-flex">
          <Button
            size="sm"
            variant="primary"
            disabled={blocked || busy}
            onClick={() => onCompose?.("instruct")}
          >
            Send instruction
          </Button>
        </span>
        <span data-testid="lane-interrupt" className="inline-flex">
          <Button
            size="sm"
            disabled={writeBlock != null || busy || lane.state === "shipped"}
            onClick={() => setDialog("interrupt")}
          >
            Interrupt
          </Button>
        </span>
        {lane.state === "fenced" && (
          <span data-testid="lane-unfence" className="inline-flex">
            <Button
              size="sm"
              variant="primary"
              disabled={writeBlock != null || busy}
              onClick={() => setDialog("unfence")}
            >
              Unfence
            </Button>
          </span>
        )}
        <span data-testid="lane-reassign" className="inline-flex">
          <Button
            size="sm"
            variant={lane.state === "quota" ? "primary" : "secondary"}
            disabled={writeBlock != null || busy || lane.state === "shipped"}
            onClick={() => setDialog("reassign")}
          >
            Reassign
          </Button>
        </span>
      </div>
      {dialog === "interrupt" && (
        <InterruptDialog
          busy={busy}
          onCancel={() => setDialog(null)}
          onInterrupt={() => run(() => api.laneInterrupt(issue))}
          onStop={() => run(() => api.laneStop(issue))}
        />
      )}
      {dialog === "unfence" && (
        <UnfenceDialog
          busy={busy}
          onCancel={() => setDialog(null)}
          onSubmit={(status, note, resume) =>
            run(() => api.laneUnfence(issue, { status, note, resume }))
          }
        />
      )}
      {dialog === "reassign" && (
        <ReassignDialog
          providers={providers}
          busy={busy}
          onCancel={() => setDialog(null)}
          onSubmit={(draft) => run(() => api.laneReassign(issue, reassignBody(draft)))}
        />
      )}
    </section>
  );
}

function InterruptDialog({
  busy,
  onCancel,
  onInterrupt,
  onStop,
}: {
  busy: boolean;
  onCancel: () => void;
  onInterrupt: () => void;
  onStop: () => void;
}) {
  return (
    <div data-testid="lane-interrupt-dialog" className="card">
      <p>Interrupt ends the current turn and leaves the lane claimed. Stop halts the agent and keeps queued messages.</p>
      <div className="flex gap-2">
        <Button size="sm" variant="ghost" onClick={onCancel} disabled={busy}>Cancel</Button>
        <span data-testid="lane-interrupt-confirm" className="inline-flex">
          <Button size="sm" onClick={onInterrupt} disabled={busy}>Interrupt turn</Button>
        </span>
        <span data-testid="lane-stop-confirm" className="inline-flex">
          <Button size="sm" variant="danger" onClick={onStop} disabled={busy}>Stop agent</Button>
        </span>
      </div>
    </div>
  );
}

function UnfenceDialog({
  busy,
  onCancel,
  onSubmit,
}: {
  busy: boolean;
  onCancel: () => void;
  onSubmit: (status: "interrupted" | "completed" | "failed", note: string, resume: boolean) => void;
}) {
  const [status, setStatus] = useState<"interrupted" | "completed" | "failed" | "">("");
  const [note, setNote] = useState("");
  const [resume, setResume] = useState(true);
  return (
    <div data-testid="lane-unfence-dialog" className="card">
      <p>History stays. The turn is not replayed.</p>
      {UNFENCE_CHOICES.map((choice) => (
        <label key={choice.status} className="flex gap-2">
          <input
            type="radio"
            name="lane-unfence-status"
            data-testid={`lane-unfence-${choice.status}`}
            checked={status === choice.status}
            onChange={() => setStatus(choice.status)}
          />
          <span>{choice.status} — {choice.blurb}</span>
        </label>
      ))}
      <input
        className="num"
        data-testid="lane-unfence-note"
        value={note}
        placeholder="Optional note"
        onChange={(e) => setNote(e.target.value)}
      />
      <label className="flex gap-2">
        <input
          type="checkbox"
          data-testid="lane-unfence-resume"
          checked={resume}
          onChange={(e) => setResume(e.target.checked)}
        />
        Resume the agent after unfence
      </label>
      <div className="flex gap-2">
        <Button size="sm" variant="ghost" onClick={onCancel} disabled={busy}>Cancel</Button>
        <span data-testid="lane-unfence-confirm" className="inline-flex">
          <Button
            size="sm"
            variant="primary"
            disabled={busy || !unfenceReady(status)}
            onClick={() => status && onSubmit(status, note, resume)}
          >
            Unfence
          </Button>
        </span>
      </div>
    </div>
  );
}

function ReassignDialog({
  providers,
  busy,
  onCancel,
  onSubmit,
}: {
  providers: LaneProvider[];
  busy: boolean;
  onCancel: () => void;
  onSubmit: (draft: { provider: string; model: string; effort: string; note: string }) => void;
}) {
  const [provider, setProvider] = useState(providers[0]?.id ?? "");
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState("");
  const [note, setNote] = useState("");
  const spec = providers.find((p) => p.id === provider);
  const models = spec?.models;
  return (
    <div data-testid="lane-reassign-dialog" className="card">
      <p>The worktree stays. The current agent stops after the new one is dispatched.</p>
      <label className="block" data-testid="lane-reassign-provider">
        Provider
        <Select
          full
          className="mt-1"
          value={provider}
          disabled={busy}
          aria-label="Provider"
          placeholder="Provider"
          onChange={(next) => {
            setProvider(next);
            setModel("");
            setEffort("");
          }}
          options={providers.map((p) => ({ value: p.id, label: p.id }))}
        />
      </label>
      {Array.isArray(models) && models.length > 0 && (
        <label className="block" data-testid="lane-reassign-model">
          Model
          <Select
            full
            className="mt-1"
            value={model}
            disabled={busy}
            aria-label="Model"
            placeholder="Select"
            onChange={setModel}
            options={modelChoices(provider, models)}
          />
        </label>
      )}
      {models === null && (
        <label>
          Model
          <input
            className="num"
            data-testid="lane-reassign-model"
            value={model}
            onChange={(e) => setModel(e.target.value)}
          />
        </label>
      )}
      {spec?.efforts && spec.efforts.length > 0 && (
        <label className="block" data-testid="lane-reassign-effort">
          Effort
          <Select
            full
            className="mt-1"
            value={effort}
            disabled={busy}
            aria-label="Effort"
            placeholder="Default"
            onChange={setEffort}
            options={effortChoices(spec.efforts)}
          />
        </label>
      )}
      <span className="chip" data-testid="lane-reassign-cost">{costLabel(provider, model) || "—"}</span>
      <input
        data-testid="lane-reassign-note"
        value={note}
        placeholder="Continuity note"
        onChange={(e) => setNote(e.target.value)}
      />
      <div className="flex gap-2">
        <Button size="sm" variant="ghost" onClick={onCancel} disabled={busy}>Cancel</Button>
        <span data-testid="lane-reassign-confirm" className="inline-flex">
          <Button
            size="sm"
            variant="primary"
            disabled={busy || !provider}
            onClick={() => onSubmit({ provider, model, effort, note })}
          >
            Reassign
          </Button>
        </span>
      </div>
    </div>
  );
}
