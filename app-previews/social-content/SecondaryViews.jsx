import { Fragment } from "react";

// Display descriptors only: these screens never read or change the app store.
const automations = [
  {
    id: "scan-sources",
    title: "Scan sources",
    note: "New source posts land in Library, marked new.",
    trigger: "Every 6 hours",
    state: "On in fixture",
    last: "3 new posts · 2h ago",
    limit: "6 drafts · HK$5 / day",
    approval: "Standing: scan",
  },
  {
    id: "social-localize",
    title: "Evening draft pass",
    note: "Draft from new sources within the daily limit.",
    trigger: "Mon–Fri · 18:00 HKT",
    state: "Paused in fixture",
    last: "Never run",
    limit: "4 drafts · HK$12 / day",
    approval: "Standing: drafting · no sends",
  },
  {
    id: "weekly-digest",
    title: "Friday schedule digest",
    note: "Collect ready posts into one approval card.",
    trigger: "Fri · 09:00 HKT",
    state: "On in fixture",
    last: "Digest staged · fixture",
    limit: "7 drafts · HK$2 / day",
    approval: "Sends always wait for approval",
  },
  {
    id: "verify-receipts",
    title: "Verify receipts",
    note: "Compare a publish receipt with its approved revision.",
    trigger: "After each publish",
    state: "On in fixture",
    last: "Mismatch illustrated · fixture",
    limit: "1 check · HK$1 / day",
    approval: "Verification only · no sends",
  },
];
const workflows = [
  {
    id: "social-localize",
    title: "Localise and schedule social posts",
    steps: [
      "adapt → writer",
      "visuals → designer",
      "review → editor",
      "schedule → publisher",
      "verify → analyst",
    ],
    note: "Selected sources become individual post records; every send requires approval.",
  },
  {
    id: "revise",
    title: "Ask the agent — revise one post",
    steps: ["change → writer / designer"],
    note: "A requested change creates a revision, with a diff and undo.",
  },
  {
    id: "scan-sources",
    title: "Scan source feeds for new posts",
    steps: ["pull → scout"],
    note: "Unseen source records enter the Library. Source text is quoted material, never instructions.",
  },
  {
    id: "weekly-digest",
    title: "Collect ready posts into a digest",
    steps: ["collect → publisher", "present → you"],
    note: "Stage one approval card per brand. Nothing sends until you approve the pinned revision.",
  },
  {
    id: "verify-receipts",
    title: "Verify publish receipts",
    steps: ["fetch receipt → publisher", "compare → analyst"],
    note: "A receipt mismatch belongs in Needs you; it never silently changes the approved post.",
  },
  {
    id: "blog-post",
    title: "Write a blog post",
    steps: [
      "brief → strategist",
      "draft → writer",
      "images → designer",
      "review → editor",
    ],
    note: "Outside the drafting standing approval: review the plan before work starts.",
  },
];
const brands = [
  {
    id: "kura",
    name: "Kura Ramen",
    handle: "@kura.ramen.hk",
    accounts: ["Instagram", "Facebook"],
    terms: ["Kura Ramen", "蔵", "HK$128", "#KuraHK"],
    disclaimer: "All prices in HKD",
    limit: "20 drafts / day",
    destinations: "Instagram, Facebook",
  },
  {
    id: "velvet",
    name: "Velvet Padel",
    handle: "@velvet.padel",
    accounts: ["Instagram", "Web source"],
    terms: ["Velvet Padel", "HK$380", "#VelvetPadel"],
    disclaimer: "No disclaimer set",
    limit: "10 drafts / day",
    destinations: "Instagram",
  },
];
const stack = { display: "grid", gap: 12 };
const row = { display: "flex", alignItems: "center", flexWrap: "wrap", gap: 8 };
const compact = { padding: 14, minWidth: 0 };
const copy = {
  margin: "6px 0",
  fontSize: "var(--text-label)",
  color: "var(--color-ink-400)",
};
const unavailable =
  "Read-only preview: automation execution and configuration are not connected.";

function DisabledAction({ children, reason = unavailable, primary = false }) {
  return (
    <button
      type="button"
      className={`btn btn-sm${primary ? " btn-primary" : ""}`}
      disabled
      title={reason}
      aria-label={`${children} — ${reason}`}
    >
      {children}
    </button>
  );
}
function Facts({ entries }) {
  return (
    <dl
      style={{
        display: "grid",
        gridTemplateColumns: "minmax(72px, 100px) minmax(0, 1fr)",
        gap: "7px 12px",
        margin: "12px 0",
        fontSize: "var(--text-label)",
      }}
    >
      {entries.map(([label, value]) => (
        <Fragment key={label}>
          <dt className="muted">{label}</dt>
          <dd style={{ margin: 0, overflowWrap: "anywhere" }}>{value}</dd>
        </Fragment>
      ))}
    </dl>
  );
}
function Heading({ title, note, children }) {
  return (
    <header className="page-head">
      <div style={row}>
        <h2 style={{ marginRight: "auto" }}>{title}</h2>
        {children}
      </div>
      <p style={copy}>{note}</p>
    </header>
  );
}
function AutomationView({ scope }) {
  return (
    <section>
      <Heading
        title="Automations"
        note="Scheduled and event triggers propose work. Sends still wait for your approval."
      >
        <DisabledAction>New automation</DisabledAction>
      </Heading>
      <p className="kicker">
        {scope} · illustrative schedules · no jobs are running
      </p>
      <div style={{ ...stack, maxWidth: 900 }}>
        {automations.map((item) => (
          <article key={item.id} className="card" style={compact}>
            <div style={row}>
              <h3 style={{ marginRight: "auto" }}>{item.title}</h3>
              <span className="chip tone">{item.state}</span>
              <span className="num" style={{ fontSize: "var(--text-label)" }}>
                {item.trigger}
              </span>
            </div>
            <p style={copy}>{item.note}</p>
            <Facts
              entries={[
                ["workflow", item.id],
                [
                  "scope",
                  item.id === "verify-receipts"
                    ? "All brands · optional context"
                    : scope,
                ],
                ["limits", item.limit],
                ["approval", item.approval],
                ["last fixture", item.last],
              ]}
            />
            <div style={row}>
              <DisabledAction>Run now</DisabledAction>
              <DisabledAction>
                {item.state.startsWith("Paused") ? "Resume" : "Pause"}
              </DisabledAction>
              <DisabledAction>Edit automation</DisabledAction>
            </div>
          </article>
        ))}
      </div>
      <p style={copy}>{unavailable}</p>
    </section>
  );
}
function WorkflowView() {
  const reason =
    "Read-only preview: workflow editing and execution are not connected.";
  return (
    <section>
      <Heading
        title="Workflows"
        note="Plan templates bundled with Social Content. Each run works over records."
      />
      <div style={{ ...stack, maxWidth: 980 }}>
        {workflows.map((item) => (
          <article key={item.id} className="card" style={compact}>
            <div style={row}>
              <h3>{item.title}</h3>
              <span className="chip tone">{item.id}</span>
              <DisabledAction reason={reason}>Edit workflow</DisabledAction>
            </div>
            <p className="kicker" style={{ overflowWrap: "anywhere" }}>
              workflows/{item.id}.md
            </p>
            <ol
              aria-label={`${item.title} steps`}
              style={{
                ...row,
                listStyle: "none",
                padding: 0,
                margin: "10px 0",
              }}
            >
              {item.steps.map((step, index) => (
                <li key={step} style={row}>
                  {index > 0 && (
                    <span className="muted" aria-hidden="true">
                      →
                    </span>
                  )}
                  <span className="chip tone">{step}</span>
                </li>
              ))}
            </ol>
            <p style={copy}>{item.note}</p>
          </article>
        ))}
      </div>
      <p style={copy}>{reason}</p>
    </section>
  );
}
function SettingsView({ selected, scope }) {
  const accountReason =
    "Read-only preview: account connection requires an authenticated connector flow; no account is connected here.";
  const saveReason =
    "Read-only preview: protected terms and drafting policy cannot be saved.";
  return (
    <section>
      <Heading
        title="Settings"
        note="Brand identity, protected terms and drafting policy. Brand context is optional."
      />
      <p className="kicker">{scope} · fixture settings</p>
      <div style={stack}>
        {selected.map((brand) => (
          <section key={brand.id} className="card" style={compact}>
            <div style={row}>
              <h3 style={{ marginRight: "auto" }}>{brand.name}</h3>
              <span className="chip tone">{brand.handle}</span>
              <span className="badge plain">fixture</span>
            </div>
            <div
              style={{
                display: "grid",
                gridTemplateColumns:
                  "repeat(auto-fit, minmax(min(100%, 280px), 1fr))",
                gap: 20,
                marginTop: 14,
              }}
            >
              <div>
                <p className="slabel">Accounts and sources</p>
                <div style={{ overflowX: "auto" }}>
                  <table className="tbl">
                    <caption
                      className="muted"
                      style={{ textAlign: "left", marginBottom: 8 }}
                    >
                      Account descriptors — not live connections
                    </caption>
                    <thead>
                      <tr>
                        <th scope="col">Channel</th>
                        <th scope="col">Status</th>
                      </tr>
                    </thead>
                    <tbody>
                      {brand.accounts.map((account) => (
                        <tr key={account}>
                          <th scope="row">{account}</th>
                          <td>
                            <span className="chip tone">Fixture only</span>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                <DisabledAction reason={accountReason}>
                  Connect account
                </DisabledAction>
                <p style={copy}>{accountReason}</p>
              </div>
              <div>
                <p className="slabel">Protected terms</p>
                <p style={copy}>
                  Terms found in a source must survive into the caption
                  verbatim.
                </p>
                <ul
                  style={{
                    ...row,
                    listStyle: "none",
                    padding: 0,
                    margin: "10px 0",
                  }}
                >
                  {brand.terms.map((term) => (
                    <li key={term} className="chip tone">
                      {term}
                    </li>
                  ))}
                </ul>
                <label>
                  Terms
                  <input
                    className="field"
                    readOnly
                    value={brand.terms.join(", ")}
                    aria-label={`${brand.name} protected terms — read only`}
                  />
                </label>
                <DisabledAction reason={saveReason} primary>
                  Save protected terms
                </DisabledAction>
                <p style={copy}>{saveReason}</p>
              </div>
              <div>
                <p className="slabel">Drafting policy</p>
                <Facts
                  entries={[
                    ["disclaimer", brand.disclaimer],
                    ["timezone", "Asia/Hong_Kong"],
                    ["draft limit", brand.limit],
                    ["destinations", brand.destinations],
                    ["approval", "Drafting only · no sends"],
                  ]}
                />
                <p style={copy}>
                  A send requires approval of the exact caption, media and
                  destination. Standing drafting approval does not authorise
                  publishing.
                </p>
              </div>
            </div>
          </section>
        ))}
        {!selected.length && (
          <div className="empty">
            <span className="etitle">No fixture settings for this context</span>
            <span>
              Select All brands, Kura Ramen or Velvet Padel to inspect the
              preview.
            </span>
          </div>
        )}
      </div>
    </section>
  );
}

export default function SecondaryViews({ view, context = "all" }) {
  const selected =
    context === "all"
      ? brands
      : brands.filter(
          (brand) => brand.name === context || brand.id === context,
        );
  const scope =
    context === "all" ? "All brands" : (selected[0]?.name ?? String(context));
  if (view === "automations") return <AutomationView scope={scope} />;
  if (view === "workflows") return <WorkflowView />;
  if (view === "settings")
    return <SettingsView selected={selected} scope={scope} />;
  return null;
}
