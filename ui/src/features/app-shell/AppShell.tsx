import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import { streamInto } from "../../lib/sse";
import { useQuery } from "../../lib/useResource";
import { navigate, useHref } from "../../lib/useLocation";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import { MASTER } from "../home/master";
import {
  addPending,
  lastSeq,
  newMessageId,
  reduceFrame,
  settlePending,
  stepSummary,
  threadItems,
} from "../home/thread";
import type { Viewer } from "../projects/work";
import { workspaceApps, type AppContext, type Installation } from "../workspace-apps/workspaceApps";
import { initialContext, rememberedContext, rememberContext, subscribeContext } from "../workspace-apps/contextSelection";
import CrmOutlet, { type CrmSection, type OutletView } from "./CrmOutlet";
import ChatCsvImport from "./ChatCsvImport";
import { assertRecordId, type HostScope } from "./hostActions";
import { isDev } from "../../env";
import AppViewContractPreview, { contractPreviewHref, contractPreviewKey } from "./app-views/AppViewContractPreview";
import "./app-shell.css";

/**
 * The trusted shared App shell (CAD-802): host-owned board surface for
 * every installed App. The board keeps its header/sidebar (App.tsx); the
 * shell adds nested Apps → installed-App navigation, the persistent
 * master conversation on the left with an accessible left drawer at
 * narrow widths, and a generic list/detail/new outlet on the right.
 *
 * One ChatPane instance lives for the shell's lifetime — desktop pane
 * and narrow drawer are one node restyled by CSS, so there is exactly
 * one draft and one SSE subscription, and closing the drawer (which
 * hides it from the tab order via `visibility`) never loses the draft.
 * The shell stays mounted across installation switches, so the chat
 * survives navigation while outlet state resets.
 *
 * Installation and context come from the trusted route plus verified
 * HTTP receipts; record links are context-bound (`ctx` + `record`).
 * The selected record is chat context only — never an authorization
 * claim. Switching installation or context clears the selection, the
 * New view and unsaved drafts. Social-content owns its context
 * selector internally, so the shell shows none there and never a
 * second, divergent one. No App-provided JavaScript runs here.
 */
/** Verified installation identity for the board-level App menu.
 *  Reported only from the shell's HTTP receipt — never the bare
 *  route — so a forged installId cannot conjure menu entries. */
export interface ActiveInstallation {
  installId: string;
  kind: string;
  title: string;
}

export default function AppShell({
  installId,
  viewer,
  children,
  onInstallation,
}: {
  installId: string;
  viewer: Viewer;
  /** Social-content's existing screen; other Apps use the generic outlet. */
  children?: React.ReactNode;
  /** Board menu wiring: receives the verified installation (or null
   *  while loading, failed, or switched away). The parent matches
   *  installId to its route and compares values before storing. */
  onInstallation?: (info: ActiveInstallation | null) => void;
}) {
  const href = useHref();
  const query = useMemo(() => new URLSearchParams(href.split("?")[1] ?? ""), [href]);
  const [installation, setInstallation] = useState<Installation | null>(null);
  const [contexts, setContexts] = useState<AppContext[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [contextId, setContextId] = useState("");
  const [linkNotice, setLinkNotice] = useState<string | null>(null);
  // Social-content's picker owns its selection; the shell observes it
  // in the same tab so chat sends carry the current scope, never the
  // one from the shell's last render.
  const [socialContext, setSocialContext] = useState<string | null>(null);
  // Outlet state lives in the URL (`ctx`, `appview`, `record`) so
  // direct links and browser back keep scope.
  const view: OutletView = query.get("appview") === "new" ? "new" : "list";
  const recordId = query.get("record");
  // CRM nested sections (CAD-784): Apps → CRM → Customers/Segments/
  // Campaigns. Only the CRM outlet reads this — every other App keeps
  // its generic list/detail/new shape untouched.
  const rawSection = query.get("crm");
  const crmSection: CrmSection =
    rawSection === "segments" || rawSection === "campaigns" ? rawSection : "customers";
  const [chatOpen, setChatOpen] = useState(false);
  // CAD-861: the app-views/v1 contract preview is a dev-only overlay
  // keyed by `contract-preview` in the URL — it never mounts in a
  // production bundle and never replaces the trusted outlet by default.
  const previewKey = isDev ? contractPreviewKey(query) : null;
  const chatPaneRef = useRef<HTMLDivElement | null>(null);
  const chatOpenRef = useRef<HTMLButtonElement | null>(null);
  // Installation switches reset outlet state but keep the chat: the
  // first mount preserves direct links, later switches strip them.
  const firstInstall = useRef(installId);
  const handledQuery = useRef<string | undefined>(undefined);

  // Every internal query write marks the resulting key as handled, so
  // the adoption effect below only answers external URL changes
  // (direct links, browser back/forward) — never our own writes.
  const queryKey = (
    ctx: string | null,
    record: string | null,
    appview: string | null,
    crm: string | null,
  ) => `${installId}|${ctx ?? ""}|${record ?? ""}|${appview ?? ""}|${crm ?? ""}`;
  const writeQuery = useCallback(
    (
      patch: {
        ctx?: string | null;
        appview?: OutletView | null;
        record?: string | null;
        crm?: CrmSection | null;
        clearContractPreview?: boolean;
      },
      opts?: { replace?: boolean },
    ) => {
      const [path, search] = href.split("?");
      const q = new URLSearchParams(search ?? "");
      if (patch.ctx !== undefined) {
        if (patch.ctx === null || patch.ctx === "") q.delete("ctx");
        else q.set("ctx", patch.ctx);
      }
      if (patch.appview !== undefined) {
        if (patch.appview === null || patch.appview === "list") q.delete("appview");
        else q.set("appview", patch.appview);
      }
      if (patch.record !== undefined) {
        if (patch.record === null) q.delete("record");
        else q.set("record", patch.record);
      }
      if (patch.crm !== undefined) {
        if (patch.crm === null || patch.crm === "customers") q.delete("crm");
        else q.set("crm", patch.crm);
      }
      if (patch.clearContractPreview) {
        q.delete("contract-preview");
        q.delete("contract-preview-view");
      }
      handledQuery.current = queryKey(q.get("ctx"), q.get("record"), q.get("appview"), q.get("crm"));
      const s = q.toString();
      navigate(path + (s ? `?${s}` : ""), { replace: opts?.replace });
    },
    [href, installId],
  );

  // Verified installation/context receipts. An operator-only read: an
  // unproven viewer sees the sign-in note, never the records.
  useEffect(() => {
    if (!viewer.operator) {
      setLoading(false);
      return;
    }
    const controller = new AbortController();
    setLoading(true);
    setLoadError(null);
    Promise.all([
      workspaceApps.detail(installId, controller.signal),
      workspaceApps.contexts(installId, controller.signal),
    ])
      .then(([next, nextContexts]) => {
        if (controller.signal.aborted) return;
        setInstallation(next);
        setContexts(nextContexts);
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) {
          setLoadError(e instanceof Error ? e.message : "Could not load this app");
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [installId, viewer.operator]);

  // Board-level App menu identity: report the verified receipt (or
  // null while it is loading, failed, or belongs to another install)
  // so the shared sidebar/phone menu can nest this installation's
  // sections. Clearing on cleanup keeps a departed installation from
  // lingering in host navigation.
  useEffect(() => {
    if (!onInstallation) return;
    if (installation && installation.install_id === installId) {
      onInstallation({
        installId,
        kind: installation.name,
        title: installation.title || installation.name,
      });
    } else {
      onInstallation(null);
    }
    return () => onInstallation(null);
  }, [installId, installation, onInstallation]);

  useEffect(() => {
    setSocialContext(rememberedContext(installId));
    return subscribeContext((changed, next) => {
      if (changed === installId) setSocialContext(next);
    });
  }, [installId]);

  // Installation switch: stale outlet state must never follow the
  // operator across the boundary. The first mount keeps direct links.
  useEffect(() => {
    if (firstInstall.current === installId) return;
    firstInstall.current = installId;
    handledQuery.current = undefined;
    setInstallation(null);
    setContexts([]);
    setLoadError(null);
    setLinkNotice(null);
    setContextId("");
    writeQuery({ ctx: null, appview: null, record: null, crm: null, clearContractPreview: true }, { replace: true });
    // The strip marks the emptied query handled: unmark so adoption
    // still runs once the new installation's contexts load.
    handledQuery.current = undefined;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [installId]);

  const activeIds = useMemo(
    () => contexts.filter((c) => c.state === "active").map((c) => c.id),
    [contexts],
  );

  // Adopt the URL's context once contexts load, and on later external
  // URL changes (browser back). A linked context must be active here;
  // a stale, inactive or ambiguous link clears the selection with a
  // notice instead of guessing. Malformed record ids are stripped.
  useEffect(() => {
    // The loaded receipts must belong to this installation: on a
    // switch commit the state still holds the previous install while
    // the URL already names the next one — adopting there would clear
    // or poison the wrong scope.
    if (loading || installation === null || installation.install_id !== installId) return;
    // Social-content owns its context end to end (the shell renders
    // no selector there): adoption must not read, write, or clear its
    // remembered selection.
    if (installation.name === "social-content") return;
    const urlCtx = query.get("ctx");
    const urlRecord = query.get("record");
    const urlView = query.get("appview");
    const urlCrm = query.get("crm");
    // A rewrite this pass emits is still "external" until adoption has
    // run against it: marking only the pre-write URL handled and
    // returning would let the next pass re-enter, and marking the
    // rewritten URL handled would make the next pass return early and
    // never adopt the surviving ctx (the cold deep-link defect). So
    // adoption is fall-through, not early-return: normalize each
    // param, then act on the still-valid remainder.
    if (handledQuery.current === queryKey(urlCtx, urlRecord, urlView, urlCrm)) return;
    handledQuery.current = queryKey(urlCtx, urlRecord, urlView, urlCrm);
    // An unknown CRM section never renders: strip it back to the
    // default instead of guessing a section. The explicit default
    // (`crm=customers`) is already canonical, so it is not rewritten.
    const badCrm = urlCrm !== null && urlCrm !== "customers" && urlCrm !== "segments" && urlCrm !== "campaigns";
    let badRecord = false;
    if (urlRecord !== null) {
      try {
        assertRecordId(urlRecord);
      } catch {
        badRecord = true;
      }
    }
    const staleCtx = urlCtx !== null && !activeIds.includes(urlCtx);
    const scopelessRecord = urlCtx === null && urlRecord !== null && !badRecord;
    if (staleCtx) {
      setContextId(fallbackContext());
      setLinkNotice(
        installation.name === "crm"
          ? "This link names a scope that is not active in this CRM installation, so nothing was opened. Ask an administrator to review the installation's CRM setup."
          : "The linked context is not active in this installation — the selection was cleared.",
      );
    } else if (scopelessRecord) {
      // A record link without scope is ambiguous: refuse it with a
      // notice rather than guessing which context it names.
      setLinkNotice(
        installation.name === "crm"
          ? "This link does not say which scope the record belongs to, so nothing was opened. Ask an administrator to review the installation's CRM setup."
          : "The record link names no context — the selection was cleared.",
      );
      setContextId(fallbackContext());
    } else if (urlCtx !== null) {
      // The URL's ctx+record/appview are adopted verbatim: they were
      // authored together — a deep link, a scoped-entry link, or a
      // history entry — never split or cleared here. Stale scope is
      // prevented at the transition origin (the scoped-entry links
      // emit a bare `?ctx=` and the section links drop record/appview),
      // not by guessing which arriving params are intentional.
      setContextId(urlCtx);
      rememberContext(installId, urlCtx);
      setLinkNotice(null);
    } else {
      setContextId(fallbackContext());
    }
    // One normalized write emits every strip at once. Marking the
    // pre-write URL handled (done above) plus this write's own
    // handled mark leaves the surviving ctx adoptable on the next
    // pass — but adoption already ran on it above, so no second
    // effect turn is needed and the URL settles in a single replace.
    if (badCrm || badRecord || staleCtx || scopelessRecord) {
      writeQuery(
        {
          crm: badCrm ? null : undefined,
          record: badRecord || staleCtx || scopelessRecord ? null : undefined,
          ctx: staleCtx ? null : undefined,
          appview: staleCtx ? null : undefined,
        },
        { replace: true },
      );
    }
  }, [loading, installation, activeIds, query, installId, writeQuery]);

  // The default selection, without persisting an empty choice when
  // this installation has no active contexts to choose from.
  const fallbackContext = () =>
    activeIds.length > 0 ? initialContext(installId, activeIds) : "";

  // The context picker is removed (operator review): context follows
  // the URL, not a selector.

  // Narrow drawer focus: opening moves into the pane, closing returns
  // to the trigger. The closed drawer is `visibility: hidden`, so it
  // stays out of the tab order with the draft intact.
  //
  // Observed defect (baseline in d379ae54, real Chrome): a synchronous
  // commit-phase `.focus()` ran before the open activation's own focus
  // (the toggle is focused on mousedown / Enter) was applied, so the
  // composer never received focus. A single rAF still fired too early —
  // before `data-open` propagated. Deferring two frames clears the open
  // commit in both real and synthesized input; the `data-open` guard
  // means the callback never focuses a still-hidden pane. Both frame
  // ids are tracked so cleanup cancels whichever is still pending — a
  // rapid close can never refocus once the user has moved on. A
  // disabled (read-only) composer yields to the enabled Close control.
  useEffect(() => {
    if (!chatOpen) return;
    let inner = 0;
    const focusComposer = () => {
      const pane = chatPaneRef.current;
      if (!pane || !pane.hasAttribute("data-open")) return;
      const composer = pane.querySelector<HTMLElement>("textarea");
      const target = composer && !composer.hasAttribute("disabled")
        ? composer
        : pane.querySelector<HTMLElement>(".app-shell-chat-close") ?? composer;
      target?.focus();
    };
    const outer = requestAnimationFrame(() => {
      inner = requestAnimationFrame(focusComposer);
    });
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setChatOpen(false);
    };
    addEventListener("keydown", onKey);
    return () => {
      cancelAnimationFrame(outer);
      cancelAnimationFrame(inner);
      removeEventListener("keydown", onKey);
      chatOpenRef.current?.focus();
    };
  }, [chatOpen]);

  // The App binding for chat sends: install plus the concrete context
  // the shell owns (generic outlet) or the workspace screen owns
  // (social, observed live via subscription). Empty context sends
  // plain chat — there is no App scope to bind. A selected context
  // that is no longer active blocks the send early with a clear
  // message; the server re-proves every binding on send regardless.
  const binding = chatBinding({
    installId,
    wanted: (installation?.name === "social-content" ? socialContext || "" : contextId),
    known: installation !== null && !loading && loadError === null,
    activeIds,
  });
  const scope: HostScope = { installId, contextId };
  // CAD-813: the Campaigns page mints assistant proposal requests
  // against the operator's most recent chat message stamped by the
  // daemon with exactly this scope. The id travels as ordinary
  // shell state — read back from the shared master-thread store,
  // never a global — so a message bound to another install or
  // context can never mint here.

  const title = installation?.title || installation?.name || "App";
  const isSocial = installation !== null && installation.name === "social-content";

  return (
    <div className="app-shell" data-app-shell-outlet={installId}>
      <div className="app-shell-crumb">
        <Link href="/apps" className="lnk text-label">
          ← All apps
        </Link>
        <span className="truncate text-ink-100 text-cardtitle" aria-current="page">
          {loading ? "Loading…" : title}
        </span>
        <span className="flex-1" />
        {isDev && installation !== null && (
          <a
            href={contractPreviewHref(href, previewKey === null ? "crm" : null)}
            className="lnk text-label app-shell-preview-toggle"
            data-contract-preview-toggle
            onClick={(e) => {
              e.preventDefault();
              navigate(contractPreviewHref(href, previewKey === null ? "crm" : null));
            }}
          >
            {previewKey === null ? "Contract preview (dev)" : "Exit contract preview"}
          </a>
        )}
        <button
          ref={chatOpenRef}
          type="button"
          className="btn btn-secondary btn-sm app-shell-chat-toggle"
          aria-expanded={chatOpen}
          aria-controls="app-shell-chat"
          onClick={() => setChatOpen((o) => !o)}
        >
          Assistant chat
        </button>
      </div>

      <div className="app-shell-grid">
        <div
          id="app-shell-chat"
          ref={chatPaneRef}
          className="app-shell-chat"
          aria-label="Assistant chat"
          data-open={chatOpen || undefined}
        >
          <div className="app-shell-chat-head">
            <strong className="text-cardtitle text-ink-100">Assistant</strong>
            <button
              type="button"
              className="btn btn-secondary btn-sm app-shell-chat-close"
              onClick={() => setChatOpen(false)}
            >
              Close chat
            </button>
          </div>
          <ChatPane
            viewer={viewer}
            contextLabel={isSocial ? null : contextLabel(contexts, contextId)}
            binding={binding}
            crm={installation !== null && installation.name === "crm"}
          />
        </div>
        <section className="app-shell-outlet" aria-label={`${title} workspace`}>
          {loading && (
            <p className="card px-4 py-5 text-secondary text-ink-400" role="status">
              Loading this app…
            </p>
          )}
          {loadError && (
            <p className="card px-4 py-5 text-secondary text-fail border-fail/40" role="alert">
              {loadError}{" "}
              <Button size="sm" onClick={() => location.reload()}>
                Retry
              </Button>
            </p>
          )}
          {!loading && !installation && !loadError && !viewer.operator && (
            <main className="card px-4 py-5" aria-label="App">
              <h2 className="text-cardtitle font-medium text-ink-100">
                {title}
              </h2>
              <p className="text-secondary text-ink-400 mt-1">
                Sign in as the operator to inspect this installation.
              </p>
              <p className="mt-2">
                <Link href="/apps" className="lnk text-label">
                  ← All apps
                </Link>
              </p>
            </main>
          )}
          {installation && (
            <>
              {isSocial ? (
                <p className="num text-micro text-ink-500">
                  {installId} · {installation.version} · context is managed inside the workspace screen
                </p>
              ) : installation.name === "crm" ? (
                // Single-company CRM (CAD-1008): a bound scope shows no
                // context/scope subtitle at all — the page is the
                // company's records. Only the exceptional unbound
                // ambiguous state below names a setup action.
                contextId === "" && activeIds.length > 1 ? (
                  <p className="text-micro text-ink-500">CRM setup is required before records open.</p>
                ) : null
              ) : (
                <p className="num text-micro text-ink-500">
                  {contexts.find((c) => c.id === contextId)?.config.label ?? "No context"} · {installation.version}
                </p>
              )}
              {/* Scoped entry: a multi-context install with no linked
                  or remembered scope stays unselected rather than
                  silently picking a client. Each context is an explicit
                  `?ctx=` link — a real URL write, deep-linkable, never a
                  hidden default. The removed header picker stays gone.
                  The same links stay available after a scope is bound
                  (chosen, linked or remembered): without them the only
                  way to reach a second context would be hand-editing
                  the URL — the regression the header picker's removal
                  introduced. The bound context keeps its link but
                  carries `aria-current`, so the current scope is still
                  announced and never a dead control — and its href is
                  the current URL itself, so following it is a
                  `navigate` no-op that keeps the open record, the New
                  view and any unsaved draft instead of resetting the
                  outlet. Single-context installs keep the entry hidden
                  — no link clutter.
                  CAD-1008: a bound CRM scope shows no switch links at
                  all — the single-company surface never offers a scope
                  control. Only the exceptional unbound multi-context
                  legacy install keeps the explicit entry; Social and
                  generic Apps are unchanged. */}
              {!isSocial && activeIds.length > 1 && (installation?.name !== "crm" || contextId === "") && (
                <nav
                  className={
                    contextId === ""
                      ? "app-shell-scope card px-4 py-4"
                      : "app-shell-switch"
                  }
                  aria-label={contextId === "" ? "Choose a context" : "Switch context"}
                >
                  <p
                    className={
                      contextId === "" ? "text-label text-ink-300" : "text-label text-ink-500"
                    }
                  >
                    {contextId === ""
                      ? installation.name === "crm"
                        ? "This CRM installation is not bound to a scope yet — administrator CRM setup is required before records open."
                        : `Choose a context to open ${title}'s records.`
                      : installation.name === "crm"
                        ? "Scope:"
                        : "Context:"}
                  </p>
                  <ul
                    className={
                      contextId === ""
                        ? "app-shell-scope-list"
                        : "app-shell-scope-list app-shell-switch-list"
                    }
                  >
                    {contexts
                      .filter((c) => c.state === "active")
                      .map((c) => (
                        <li key={c.id}>
                          <Link
                            href={scopedEntryHref(href, c.id, contextId)}
                            className="lnk text-label"
                            data-scope-link={c.id}
                            aria-current={c.id === contextId ? "page" : undefined}
                          >
                            {c.config.label}
                          </Link>
                        </li>
                      ))}
                  </ul>
                </nav>
              )}
              {linkNotice && (
                <p className="card px-4 py-3 text-label text-warn border-warn/40" role="alert">
                  {linkNotice}
                </p>
              )}
              {previewKey !== null ? (
                <AppViewContractPreview
                  exampleKey={previewKey}
                  installationKind={installation.name}
                />
              ) : isSocial && children ? (
                children
              ) : (
                <CrmOutlet
                  key={`${installId}:${contextId}`}
                  scope={scope}
                  installationTitle={title}
                  appKind={installation.name === "crm" ? "crm" : "generic"}
                  view={view}
                  recordId={recordId}
                  section={crmSection}
                  viewer={viewer}
                  onView={(v) => writeQuery({ appview: v === "list" ? null : v })}
                  onSelect={(id) => writeQuery({ record: id })}
                  // Section moves ride real submenu links (CrmOutlet):
                  // their hrefs already clear the record view and
                  // drafts, so no callback is needed here.
                  // Created records land on list + details in ONE query
                  // write. Two sequential writes would each start from the
                  // stale render's href, so the second would re-apply the
                  // first's pre-image and retain `appview=new` (CAD-781 F1).
                  onRecordCreated={(id) => writeQuery({ appview: null, record: id })}
                />
              )}
            </>
          )}
        </section>
      </div>

      {chatOpen && (
        <div className="app-shell-scrim" onClick={() => setChatOpen(false)} aria-hidden="true" />
      )}
    </div>
  );
}

export interface ChatScope {
  install_id: string;
  context_id: string;
}

export interface ChatBinding {
  scope: ChatScope | null;
  error: string | null;
}

/** The chat send's App scope: empty wants plain chat, a selected but
 *  inactive context blocks early, otherwise the server proves the
 *  binding on send. Pure — unit-tested through the mounted suite. */
export function chatBinding({ installId, wanted, known, activeIds }: {
  installId: string;
  wanted: string;
  known: boolean;
  activeIds: string[];
}): ChatBinding {
  if (wanted === "") return { scope: null, error: null };
  if (known && !activeIds.includes(wanted)) {
    return {
      scope: null,
      error: `Context “${wanted}” is not active in this installation — pick a current one before sending.`,
    };
  }
  return { scope: { install_id: installId, context_id: wanted }, error: null };
}

function contextLabel(contexts: AppContext[], contextId: string): string | null {
  if (!contextId) return null;
  return contexts.find((c) => c.id === contextId)?.config.label ?? null;
}

/** A scoped-entry link: sets `ctx` and clears any carried record /
 *  new-view / section state so entering a scope never lands on the
 *  prior scope's drawer or draft. Re-entering the already-bound
 *  scope keeps the outlet exactly as it is: the link resolves to the
 *  current URL, so `navigate` no-ops and an open record, the New
 *  view and unsaved drafts survive — the link stays real (copiable,
 *  openable in a new tab), never a dead control. Pure — unit-tested
 *  via the shell. */
export function scopedEntryHref(href: string, contextId: string, boundId: string): string {
  if (contextId === boundId) return href;
  const [path, search] = href.split("?");
  const q = new URLSearchParams(search ?? "");
  q.set("ctx", contextId);
  q.delete("record");
  q.delete("appview");
  const s = q.toString();
  return path + (s ? `?${s}` : "");
}

/** The daemon-stamped App binding on an entry's payload, if verified. */
export function entryApp(payload: unknown): {
  install_id: string;
  context_id: string;
} | null {
  const app = (payload as { app?: unknown } | null)?.app;
  if (!app || typeof app !== "object") return null;
  const row = app as Record<string, unknown>;
  if (row.verified !== true || typeof row.install_id !== "string" || typeof row.context_id !== "string") {
    return null;
  }
  return { install_id: row.install_id, context_id: row.context_id };
}

/**
 * CAD-813: the most recent operator chat message the daemon stamped
 * with exactly `scope`'s verified App binding — the only
 * `message_id` a proposal-request mint may name. Entries read back
 * from the shared master-thread store are the source: a pending send
 * or a foreign-scope message never qualifies. `null` when the
 * operator has not sent a scoped message in this App yet.
 */
export function latestScopedChatMessage(
  state: { entries?: { role?: string; message?: string | null; payload?: unknown }[] } | null,
  scope: HostScope,
): string | null {
  if (scope.contextId === "") return null;
  const entries = state?.entries ?? [];
  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i];
    if (entry.role !== "operator" || typeof entry.message !== "string") continue;
    const bound = entryApp(entry.payload);
    if (bound?.install_id === scope.installId && bound.context_id === scope.contextId) {
      return entry.message;
    }
  }
  return null;
}

/**
 * The actual master conversation in compact form: the same
 * `resources.masterThread` store Home reads and writes, streamed live
 * from `/api/threads/master/stream`. Whatever the operator says here
 * lands in the same thread Home shows — one conversation, two panes.
 * Sends carry the shell's current installation/context; the daemon
 * proves both against its store and stamps the verified binding on
 * the entry — the chip below renders only that read-back stamp, never
 * what was sent.
 */
function ChatPane({
  viewer,
  contextLabel,
  binding,
  crm,
}: {
  viewer: Viewer;
  contextLabel: string | null;
  binding: ChatBinding;
  crm?: boolean;
}) {
  const thread = useQuery(resources.masterThread);
  const [draft, setDraft] = useState("");
  const [sendError, setSendError] = useState<string | null>(null);
  const loaded = thread.data !== null;

  useEffect(() => {
    if (!loaded) void resources.masterThread.refresh();
  }, [loaded]);
  useEffect(() => {
    if (!loaded || thread.data?.missing === true) return;
    const sub = streamInto(resources.masterThread, reduceFrame, {
      url: `/api/threads/${MASTER}/stream`,
      events: ["entry"],
      lastEventId: String(lastSeq(resources.masterThread.get().data) ?? 0),
      onError: () => undefined,
    });
    return () => sub.close();
  }, [loaded, thread.data?.missing]);

  const items = threadItems(thread.data);
  const tail = items.slice(-8);
  const canSend = viewer.operator && !viewer.readOnly;

  const send = () => {
    const body = draft.trim();
    if (!body || !canSend) return;
    if (binding.error !== null) {
      setSendError(binding.error);
      return;
    }
    const message = newMessageId();
    setSendError(null);
    resources.masterThread.write((s) => addPending(s, message, body, Date.now()));
    api
      .threadSend(MASTER, body, message, undefined, binding.scope ?? undefined)
      .then(() => {
        resources.masterThread.write((s) => settlePending(s, message, { ok: true }));
        void resources.masterState.refresh();
      })
      .catch((e: ApiError) => {
        setSendError(e.message ?? String(e));
        resources.masterThread.write((s) => settlePending(s, message, { ok: false, error: e.message ?? String(e) }));
      });
    setDraft("");
  };

  return (
    <div className="app-chat" data-chat-pane>
      <p className="text-micro text-ink-500">
        Master thread{contextLabel ? ` · ${contextLabel}` : ""} — context for this turn, never access proof.
      </p>
      {thread.status === "failed" && (
        <p className="text-label text-fail" role="alert">
          The thread could not be read — {thread.error}{" "}
          <button type="button" className="lnk" onClick={() => void resources.masterThread.refresh()}>
            Retry
          </button>
        </p>
      )}
      {!loaded && thread.status !== "failed" && (
        <p className="text-label text-ink-500" role="status">
          Reading the thread…
        </p>
      )}
      {loaded && tail.length === 0 && (
        <p className="text-label text-ink-500" data-empty="chat">
          No conversation yet. Ask the master from Home, or send the first message here.
        </p>
      )}
      <ol className="app-chat-list" aria-label="Recent master messages">
        {tail.map((item) => (
          <li key={item.key} className="text-secondary text-ink-300 break-words">
            <ChatRow item={item} />
          </li>
        ))}
      </ol>
      {sendError && (
        <p className="text-label text-fail" role="alert">
          {sendError}
        </p>
      )}
      {crm === true && binding.scope !== null && (
        <details className="app-chat-import">
          <summary className="text-label text-ink-300">Import a customer list</summary>
          <ChatCsvImport
            // CAD-1016: a context change remounts the import — a pending
            // plan/preview/choices from the prior scope can never bleed
            // into the new one.
            key={`${binding.scope.install_id}:${binding.scope.context_id}`}
            scope={{ installId: binding.scope.install_id, contextId: binding.scope.context_id }}
            canWrite={canSend}
            onSendIntent={async (intent) => {
              const message = newMessageId();
              const body = JSON.stringify(intent);
              try {
                await api.threadSend(MASTER, body, message, undefined, binding.scope ?? undefined);
                void resources.masterThread.refresh();
                void resources.masterState.refresh();
                return null;
              } catch (e: unknown) {
                return e instanceof ApiError ? e.message : String(e);
              }
            }}
          />
        </details>
      )}
      <form
        className="app-chat-form"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        <label className="sr-only" htmlFor="app-shell-chat-box">
          Message to the master
        </label>
        <textarea
          id="app-shell-chat-box"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              send();
            }
          }}
          rows={2}
          disabled={!canSend}
          placeholder={canSend ? "Ask Master… (Enter sends)" : "Read-only · Sending is unavailable"}
          aria-label="Message to the master"
          className="app-chat-box"
        />
        <Button type="submit" variant="primary" size="sm" disabled={!canSend || !draft.trim()}>
          Send
        </Button>
      </form>
    </div>
  );
}

function ChatRow({ item }: { item: ReturnType<typeof threadItems>[number] }) {
  if (item.type === "operator" || item.type === "pending") {
    const text = item.type === "operator" ? item.entry.text : item.pending.text;
    const bound = item.type === "operator" ? entryApp(item.entry.payload) : null;
    return (
      <p>
        <strong className="text-ink-200">You:</strong> {text}
        {bound && (
          <span className="chip ml-2" title={`Server-verified App context: ${bound.install_id}`}>
            ✓ {bound.context_id}
          </span>
        )}
      </p>
    );
  }
  if (item.type === "answer") {
    return (
      <p>
        <strong className="text-ink-200">Master:</strong> {(stepSummary(item.entry.text) || item.entry.text).slice(0, 280)}
      </p>
    );
  }
  if (item.type === "commentary") {
    return <p className="italic text-ink-400">{item.entry.text.slice(0, 280)}</p>;
  }
  if (item.type === "system") {
    return <p className="text-micro text-ink-500">· {stepSummary(item.entry.text).slice(0, 160)}</p>;
  }
  return <p className="text-micro text-ink-500">· {item.entries.length} tool steps</p>;
}
