/* ScAutomations — scheduled/event triggers running under standing
 * approvals (§5.1 row 3). Each card opens a settings drawer (trigger,
 * scope, workflow, limits, notifications, run history); "New automation"
 * starts from a template and lands in the same form. Everything saves to
 * mock state only. */
import { useState } from "react";
import type { ReactNode } from "react";
import Link from "../../../ui/Link";
import { api, describeTrigger } from "./mock/api";
import { fmt } from "./fmt";
import type { Automation, AutomationCfg, AutomationTrigger } from "./mock/data";
import ScDrawer from "./ScDrawer";
import { runState, runWhat, STATE_PILL } from "./ScRuns";
import { BTN_PRIMARY_SM, BTN_SM, PageHead } from "./widgets";

const ALL_SOURCES = ["instagram", "facebook", "web"];
const DOW = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const DOW_ORDER = [1, 2, 3, 4, 5, 6, 0];
const TIMEZONES = ["Asia/Hong_Kong", "Asia/Singapore", "Europe/London"];

const BTN_DANGER =
  "h-7 px-2.5 inline-flex items-center gap-1.5 rounded border border-fail/50 text-label text-fail hover:bg-fail/10 disabled:opacity-40";

const TEMPLATES: { key: string; name: string; desc: string; cfg: AutomationCfg }[] = [
  {
    key: "scan", name: "Scan sources",
    desc: "Pull new posts from connected sources; they land in Library marked new.",
    cfg: { trigger: { kind: "interval", hours: 6 }, scopeClients: "current",
      scopeSources: [...ALL_SOURCES], workflow: "scan-sources",
      maxDraftsPerDay: 6, budgetPerDay: 5, notifyDigest: true, notifyError: true },
  },
  {
    key: "draft", name: "Draft pass",
    desc: "Draft from new sources, up to the daily limit.",
    cfg: { trigger: { kind: "weekly", weekdays: [1, 2, 3, 4, 5], time: "18:00", timezone: "Asia/Hong_Kong" },
      scopeClients: "current", scopeSources: [...ALL_SOURCES], workflow: "social-localize",
      maxDraftsPerDay: 4, budgetPerDay: 12, notifyDigest: true, notifyError: true },
  },
  {
    key: "digest", name: "Weekly digest",
    desc: "Collect the week's ready posts into one approve card.",
    cfg: { trigger: { kind: "weekly", weekdays: [5], time: "09:00", timezone: "Asia/Hong_Kong" },
      scopeClients: "current", scopeSources: [], workflow: "weekly-digest",
      maxDraftsPerDay: 7, budgetPerDay: 2, notifyDigest: true, notifyError: true },
  },
  {
    key: "verify", name: "Verify receipts",
    desc: "Compare what went out with the approved revision after every publish.",
    cfg: { trigger: { kind: "event", event: "publish" }, scopeClients: "all",
      scopeSources: [], workflow: "verify-receipts",
      maxDraftsPerDay: 1, budgetPerDay: 1, notifyDigest: false, notifyError: true },
  },
];

function cfgErrors(name: string, cfg: AutomationCfg): Record<string, string> {
  const e: Record<string, string> = {};
  const t = cfg.trigger;
  if (!name.trim()) e.name = "give it a name";
  if (t.kind === "interval" && !(Number(t.hours) >= 1)) e.hours = "at least 1 hour";
  if (t.kind === "weekly" && !t.weekdays?.length) e.days = "pick at least one day";
  if (t.kind === "weekly" && !t.time) e.time = "pick a time";
  if (t.kind === "event" && !t.event) e.event = "pick the event";
  if (!(cfg.maxDraftsPerDay >= 1)) e.maxDrafts = "at least 1";
  if (!(cfg.budgetPerDay >= 0)) e.budget = "0 or more";
  return e;
}

function Field({ label, error, children }: { label: string; error?: string; children: ReactNode }) {
  return (
    <div className="sc-kv">
      <span className="sc-k">{label}</span>
      <span className="min-w-0">
        {children}
        {error && <span className="sc-err block mt-1">{error}</span>}
      </span>
    </div>
  );
}

function Sect({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-2">
      <span className="slabel">{label}</span>
      {children}
    </div>
  );
}

/** The settings form — shared by the Edit drawer and the new-automation
 * flow. Local state until Save. */
function AutoForm({
  auto,
  seed,
  say,
  runsHref,
  onClose,
  onBack,
}: {
  auto: Automation | null;
  seed: { name: string; cfg: AutomationCfg };
  say: (kind: "ok" | "warn" | "err", text: string) => void;
  runsHref: string;
  onClose: () => void;
  onBack?: () => void;
}) {
  const [name, setName] = useState(seed.name);
  const [cfg, setCfg] = useState<AutomationCfg>(() => structuredClone(seed.cfg));
  const [confirmDel, setConfirmDel] = useState(false);
  const errs = cfgErrors(name, cfg);
  const t = cfg.trigger;
  const setT = (p: Partial<AutomationTrigger>) =>
    setCfg((c) => ({ ...c, trigger: { ...c.trigger, ...p } }));
  const workflows = api.workflows.list();
  const wf = workflows.find((w) => w.id === cfg.workflow);
  const hist = auto ? api.automations.recentRuns(auto) : [];

  const save = () => {
    if (auto) api.automations.update(auto.id, { name, cfg });
    else api.automations.create({ name: name.trim() || seed.name, cfg });
    say("ok", auto ? `${auto.name} saved` : `${name.trim() || seed.name} created — it starts paused`);
    onClose();
  };

  return (
    <>
      {onBack && (
        <button className="lnk text-label self-start" onClick={onBack}>
          ‹ change template — {seed.name}
        </button>
      )}

      <Field label="name" error={errs.name}>
        <input className="field w-full" value={name} onChange={(e) => setName(e.target.value)} />
      </Field>

      <Sect label="when it runs">
        <div className="sc-seg self-start">
          {(["interval", "weekly", "event"] as const).map((k) => (
            <button
              key={k}
              className={t.kind === k ? "sc-on" : ""}
              onClick={() => setT({ kind: k, ...(k === "event" && !t.event ? { event: "publish" } : {}) })}
            >
              {k === "interval" ? "every N hours" : k === "weekly" ? "days + time" : "on event"}
            </button>
          ))}
        </div>
        {t.kind === "interval" && (
          <Field label="every" error={errs.hours}>
            <span className="inline-flex items-center gap-2">
              <input
                className="field w-20" type="number" min={1} value={t.hours ?? ""}
                onChange={(e) => setT({ hours: Number(e.target.value) })}
              />
              <span className="text-label text-ink-500">hours</span>
            </span>
          </Field>
        )}
        {t.kind === "weekly" && (
          <>
            <div className="flex flex-col gap-1.5">
              <div className="sc-days">
                {DOW_ORDER.map((d) => (
                  <button
                    key={d}
                    className={(t.weekdays ?? []).includes(d) ? "sc-on" : ""}
                    onClick={() =>
                      setT({
                        weekdays: (t.weekdays ?? []).includes(d)
                          ? (t.weekdays ?? []).filter((w) => w !== d)
                          : [...(t.weekdays ?? []), d],
                      })
                    }
                  >
                    {DOW[d]}
                  </button>
                ))}
              </div>
              {errs.days && <span className="sc-err">{errs.days}</span>}
            </div>
            <Field label="at" error={errs.time}>
              <span className="inline-flex items-center gap-2 flex-wrap">
                <input
                  className="field w-24" type="time" value={t.time ?? ""}
                  onChange={(e) => setT({ time: e.target.value })}
                />
                <select
                  className="field" value={t.timezone ?? "Asia/Hong_Kong"}
                  onChange={(e) => setT({ timezone: e.target.value })}
                >
                  {TIMEZONES.map((z) => <option key={z} value={z}>{z}</option>)}
                </select>
              </span>
            </Field>
          </>
        )}
        {t.kind === "event" && (
          <div className="flex flex-col gap-1.5">
            <div className="sc-seg self-start">
              {(["publish", "sources"] as const).map((ev) => (
                <button key={ev} className={t.event === ev ? "sc-on" : ""} onClick={() => setT({ event: ev })}>
                  {ev === "publish" ? "after each publish" : "when new sources arrive"}
                </button>
              ))}
            </div>
            {errs.event && <span className="sc-err">{errs.event}</span>}
          </div>
        )}
        <span className="kicker">→ {describeTrigger(t)}</span>
      </Sect>

      <Sect label="scope">
        <Field label="clients">
          <div className="sc-seg">
            <button className={cfg.scopeClients === "current" ? "sc-on" : ""}
              onClick={() => setCfg((c) => ({ ...c, scopeClients: "current" }))}>
              this client
            </button>
            <button className={cfg.scopeClients === "all" ? "sc-on" : ""}
              onClick={() => setCfg((c) => ({ ...c, scopeClients: "all" }))}>
              all clients
            </button>
          </div>
        </Field>
        <Field label="sources">
          <div className="sc-days">
            {ALL_SOURCES.map((s) => (
              <button
                key={s}
                className={cfg.scopeSources.includes(s) ? "sc-on" : ""}
                onClick={() =>
                  setCfg((c) => ({
                    ...c,
                    scopeSources: c.scopeSources.includes(s)
                      ? c.scopeSources.filter((x) => x !== s)
                      : [...c.scopeSources, s],
                  }))
                }
              >
                {s}
              </button>
            ))}
          </div>
        </Field>
      </Sect>

      <Sect label="what it does">
        <select
          className="field w-full" value={cfg.workflow}
          onChange={(e) => setCfg((c) => ({ ...c, workflow: e.target.value }))}
        >
          {workflows.map((w) => <option key={w.id} value={w.id}>{w.title}</option>)}
        </select>
        {wf && <div className="text-label text-ink-500">{wf.note}</div>}
      </Sect>

      <Sect label="limits">
        <Field label="drafts/day" error={errs.maxDrafts}>
          <input
            className="field w-20" type="number" min={1} value={cfg.maxDraftsPerDay}
            onChange={(e) => setCfg((c) => ({ ...c, maxDraftsPerDay: Number(e.target.value) }))}
          />
        </Field>
        <Field label="budget/day" error={errs.budget}>
          <span className="inline-flex items-center gap-2">
            <span className="text-label text-ink-500">HK$</span>
            <input
              className="field w-20" type="number" min={0} value={cfg.budgetPerDay}
              onChange={(e) => setCfg((c) => ({ ...c, budgetPerDay: Number(e.target.value) }))}
            />
          </span>
        </Field>
      </Sect>

      <Sect label="approval">
        <div className="card p-3 text-label text-ink-400">
          Sends always wait for your approval in the digest — this can't be
          turned off from an automation.{" "}
          <span className="num text-ink-500">{auto?.approval ?? "sends always wait — digest"}</span>
        </div>
      </Sect>

      <Sect label="notifications">
        {(
          [
            ["notifyDigest", "Include in the digest", "a line in the daily summary"],
            ["notifyError", "Tell me when it fails", "lands in Needs you"],
          ] as const
        ).map(([k, label, note]) => (
          <button
            key={k} className="sc-opt" role="checkbox" aria-checked={cfg[k]}
            onClick={() => setCfg((c) => ({ ...c, [k]: !c[k] }))}
          >
            <span className={`sc-tick ${cfg[k] ? "sc-on" : ""}`}>{cfg[k] ? "✓" : ""}</span>
            {label}
            <span className="kicker">{note}</span>
          </button>
        ))}
      </Sect>

      {auto && (
        <Sect label="run history">
          {hist.length === 0 && (
            <div className="text-label text-ink-500">
              No runs yet — history appears after its first run.
            </div>
          )}
          {hist.map((r) => {
            const st = STATE_PILL[runState(r)];
            return (
              <Link key={r.id} href={runsHref} className="sc-rhist">
                <span className={`chip ${st.cls}`}>{st.label}</span>
                <span className="text-label text-ink-200 min-w-0 flex-1 truncate">{runWhat(r)}</span>
                <span className="kicker">{fmt.ago(r.at)}</span>
              </Link>
            );
          })}
          {hist.length > 0 && (
            <Link href={runsHref} className="lnk text-label">open Runs</Link>
          )}
        </Sect>
      )}

      <div className="flex flex-wrap items-center gap-2 pt-1 border-t border-ink-700">
        {auto && (
          <>
            <button
              className={BTN_SM}
              onClick={() => {
                api.automations.runNow(auto.id);
                say("ok", `${auto.name} queued — see Runs`);
              }}
            >
              Run now
            </button>
            <button className={BTN_SM} onClick={() => api.automations.toggle(auto.id)}>
              {auto.on ? "Pause" : "Resume"}
            </button>
          </>
        )}
        <span className="flex-1" />
        {auto && (
          <button
            className={BTN_DANGER}
            onClick={() => {
              if (!confirmDel) return setConfirmDel(true);
              api.automations.remove(auto.id);
              say("warn", `${auto.name} deleted`);
              onClose();
            }}
          >
            {confirmDel ? "Confirm delete" : "Delete"}
          </button>
        )}
        <button className={BTN_PRIMARY_SM} disabled={Object.keys(errs).length > 0} onClick={save}>
          {auto ? "Save" : "Create"}
        </button>
      </div>
    </>
  );
}

export default function ScAutomations({
  say,
  runsHref,
}: {
  say: (kind: "ok" | "warn" | "err", text: string) => void;
  runsHref: string;
}) {
  const [open, setOpen] = useState<string | "new" | null>(null);
  const [tpl, setTpl] = useState<(typeof TEMPLATES)[number] | null>(null);
  const list = api.automations.list();
  const current = open && open !== "new" ? api.automations.get(open) ?? null : null;

  const close = () => {
    setOpen(null);
    setTpl(null);
  };

  return (
    <main className="px-4 lg:px-8 pt-4 pb-9">
      <PageHead title="Automations" note="scheduled/event triggers — they propose; sends still wait for the digest">
        <span className="flex-1" />
        <button className={BTN_SM} onClick={() => setOpen("new")}>New automation</button>
      </PageHead>
      <div className="flex flex-col gap-3 max-w-3xl">
        {list.map((a) => (
          <div
            key={a.id}
            className="card tcard p-3.5 flex items-start gap-3.5 cursor-pointer"
            onClick={() => setOpen(a.id)}
          >
            <button
              className={`sc-toggle ${a.on ? "sc-on" : ""} mt-0.5`}
              role="switch"
              aria-checked={a.on}
              aria-label={`${a.name} — ${a.on ? "on" : "paused"}`}
              onClick={(e) => {
                e.stopPropagation();
                api.automations.toggle(a.id);
              }}
            />
            <div className="min-w-0 flex-1">
              <div className="text-label font-medium text-ink-100">{a.name}</div>
              <div className="text-label text-ink-400 mt-0.5">{a.desc}</div>
            </div>
            <div className="text-right flex-none flex flex-col items-end gap-1">
              <div className="num text-label text-ink-300">{a.every}</div>
              <div className="kicker">last: {a.last}</div>
              <button
                className="lnk text-label"
                onClick={(e) => {
                  e.stopPropagation();
                  setOpen(a.id);
                }}
              >
                Edit ›
              </button>
            </div>
          </div>
        ))}
      </div>

      {(current || open === "new") && (
        <ScDrawer
          title={current ? current.name : tpl ? tpl.name : "New automation"}
          sub={current ? `${current.every} · ${current.on ? "on" : "paused"}` : "template → settings"}
          onClose={close}
        >
          {open === "new" && !tpl && (
            <div className="flex flex-col gap-2.5">
              <p className="text-label text-ink-400">
                Start from a template — every field stays editable after.
              </p>
              {TEMPLATES.map((t) => (
                <button key={t.key} className="card tcard p-3 text-left" onClick={() => setTpl(t)}>
                  <div className="text-label font-medium text-ink-100">{t.name}</div>
                  <div className="text-label text-ink-500 mt-0.5">{t.desc}</div>
                  <div className="kicker mt-1.5">
                    {describeTrigger(t.cfg.trigger)} · {t.cfg.workflow}
                  </div>
                </button>
              ))}
            </div>
          )}
          {open === "new" && tpl && (
            <AutoForm
              key={tpl.key}
              auto={null}
              seed={tpl}
              say={say}
              runsHref={runsHref}
              onClose={close}
              onBack={() => setTpl(null)}
            />
          )}
          {current && (
            <AutoForm
              key={current.id}
              auto={current}
              seed={{ name: current.name, cfg: current.cfg }}
              say={say}
              runsHref={runsHref}
              onClose={close}
            />
          )}
        </ScDrawer>
      )}
    </main>
  );
}
