/* SocialContent.tsx — the social-content app running inside the board.
 * A section row under the board header (SectionTabs, like Projects and
 * Settings use), a client switcher where the prototype had one, then the
 * section screen. Everything reads the mock facade — no daemon calls.
 */
import { useState } from "react";
import SectionTabs from "../../../ui/SectionTabs";
import Link from "../../../ui/Link";
import type { Route } from "../../../lib/router";
import { api, useMock } from "./mock/api";
import ScHome from "./ScHome";
import ScLibrary from "./ScLibrary";
import ScRuns from "./ScRuns";
import ScPost from "./ScPost";
import ScNeeds from "./ScNeeds";
import ScAutomations from "./ScAutomations";
import ScWorkflows from "./ScWorkflows";
import ScSettings from "./ScSettings";

export type ScSection = string;

const SECTIONS: { id: ScSection; label: string }[] = [
  { id: "home", label: "Home" },
  { id: "library", label: "Library" },
  { id: "runs", label: "Runs" },
  { id: "needs", label: "Needs you" },
  { id: "automations", label: "Automations" },
  { id: "workflows", label: "Workflows" },
  { id: "settings", label: "Settings" },
];

/** href of an app location — post routes carry the id as the third segment. */
export type ScHref = (section: ScSection | null, arg?: string | null) => string;

function ClientSwitch() {
  const [open, setOpen] = useState(false);
  const c = api.clients.current();
  return (
    <div className="relative">
      <button
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
        className="h-8 px-2.5 inline-flex items-center gap-2 rounded border border-edge bg-ink-850 text-label text-ink-200 hover:border-edge-hover"
        aria-expanded={open}
      >
        <span className="sc-avatar" style={{ background: c.color }}>{c.initials}</span>
        {c.name}
        <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.5"
          className={`transition-transform ${open ? "rotate-180" : ""}`}>
          <path d="M2 3.5l3 3 3-3" />
        </svg>
      </button>
      {open && (
        <>
          <div className="fixed inset-0 z-30" onClick={() => setOpen(false)} />
          <div className="sc-menu">
            {api.clients.list().map((cl) => (
              <button
                key={cl.id}
                onClick={() => {
                  api.clients.switch(cl.id);
                  setOpen(false);
                }}
              >
                <span className="sc-avatar" style={{ background: cl.color }}>{cl.initials}</span>
                <span>{cl.name}</span>
                <span className="sc-meta">@{cl.handle}</span>
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

/** "Write a blog post" — sits outside the drafting standing approval, so the
 * wired app opens a plan card first; the prototype shows that card. */
function BlogButton({ say }: { say: (kind: "ok" | "warn" | "err", text: string) => void }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button className={SC_BTN_SM} onClick={() => setOpen(true)}>
        Write a blog post
      </button>
      {open && (
        <div
          className="sc-scrim"
          onClick={(e) => {
            if (e.target === e.currentTarget) setOpen(false);
          }}
        >
          <div className="card reveal w-full max-w-[480px] p-4 flex flex-col gap-3">
            <h2 className="text-cardtitle font-semibold text-ink-100">Write a blog post</h2>
            <p className="text-label text-ink-400">
              This action sits outside the drafting standing approval, so wired-up it opens a plan
              card first (inputs → approve → run the blog-post workflow). In the prototype it only
              shows this card.
            </p>
            <div className="card p-3">
              <div className="text-label">
                <strong className="text-ink-100">blog-post</strong>
                <span className="text-ink-500"> — plan preview</span>
              </div>
              <div className="text-label text-ink-500 mt-1">
                input: brief → outline → draft → images → review — see workflows/blog-post.md
              </div>
            </div>
            <div className="flex justify-end gap-2">
              <button className={SC_BTN_SM} onClick={() => setOpen(false)}>Not now</button>
              <button
                className={SC_BTN_PRIMARY_SM}
                onClick={() => {
                  setOpen(false);
                  say("ok", "prototype: plan approval would dispatch the workflow here");
                }}
              >
                Approve &amp; run (mock)
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}

const SC_BTN_SM =
  "h-7 px-2.5 inline-flex items-center gap-1.5 rounded border border-ink-600 text-label text-ink-200 hover:border-accent/60 hover:text-accent disabled:opacity-40";
const SC_BTN_PRIMARY_SM =
  "h-7 px-2.5 inline-flex items-center gap-1.5 rounded bg-accent text-on-accent text-label font-medium disabled:opacity-40";

export default function SocialContent({
  route,
  hrefFor,
  say,
}: {
  route: Extract<Route, { screen: "apps" }>;
  hrefFor: (r: Route) => string;
  say: (kind: "ok" | "warn" | "err", text: string) => void;
}) {
  useMock();
  const section = route.section ?? "home";
  const needs = api.needsYou.list().length;
  const scHref: ScHref = (sec, arg = null) =>
    hrefFor({ screen: "apps", app: "social-content", section: sec, arg });

  return (
    <div className="min-w-0">
      <SectionTabs
        label="apps / social-content"
        tabs={SECTIONS.map((s) => ({
          label:
            s.id === "needs" && needs ? (
              <>
                Needs you
                <span className="chip bg-fail/10 text-fail ml-1.5 !py-0">{needs}</span>
              </>
            ) : (
              s.label
            ),
          href: scHref(s.id),
          on: section === s.id || (s.id === "home" && section === "post"),
        }))}
      />
      <div className="px-4 lg:px-8 pt-3 flex items-center gap-2">
        <ClientSwitch />
        <span className="kicker">content studio · prototype</span>
        <span className="flex-1" />
        <BlogButton say={say} />
      </div>
      {section === "home" && (
        <ScHome postHref={(id) => scHref("post", id)} needsHref={scHref("needs")} />
      )}
      {section === "library" && (
        <ScLibrary say={say} postHref={(id) => scHref("post", id)} runsHref={scHref("runs")} />
      )}
      {section === "runs" && (
        <ScRuns postHref={(id) => scHref("post", id)} needsHref={scHref("needs")} />
      )}
      {section === "post" && route.arg && (
        <ScPost id={route.arg} say={say} postHref={(id) => scHref("post", id)} />
      )}
      {section === "post" && !route.arg && (
        <main className="px-4 lg:px-8 pt-5 pb-9">
          <p className="text-secondary text-ink-400">
            No post named in the URL — <Link href={scHref("home")} className="lnk">back to Home</Link>.
          </p>
        </main>
      )}
      {section === "needs" && <ScNeeds say={say} postHref={(id) => scHref("post", id)} />}
      {section === "automations" && <ScAutomations />}
      {section === "workflows" && <ScWorkflows />}
      {section === "settings" && <ScSettings />}
      {!SECTIONS.some((s) => s.id === section) && section !== "post" && (
        <main className="px-4 lg:px-8 pt-5 pb-9">
          <h1 className="text-section font-semibold text-ink-100">Nothing lives here</h1>
          <p className="text-body text-ink-400 mt-2">
            <span className="num break-all">/apps/social-content/{section}</span> is not a screen of
            this app.{" "}
            <Link href={scHref("home")} className="lnk">
              Go to its home
            </Link>
          </p>
        </main>
      )}
    </div>
  );
}
