import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { navigate, useHref } from "../../lib/useLocation";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import type { Viewer } from "../projects/work";
import { workspaceApps, type AppContext, type Installation } from "../workspace-apps/workspaceApps";
import { initialContext, rememberedContext, rememberContext, subscribeContext } from "../workspace-apps/contextSelection";
import CrmOutlet, { type CrmSection, type OutletView } from "./CrmOutlet";
import { ErrorNotice } from "./shared/States";
import Conversation from "./chat/Conversation";
import { useAppChat } from "./chat/descriptorClient";
import type { ChatBinding, ChatScope } from "./chat/types";
import { assertRecordId, type HostScope } from "./hostActions";
import { isDev } from "../../env";
import AppViewContractPreview, { contractPreviewHref, contractPreviewKey } from "./app-views/AppViewContractPreview";
import LiveAppView from "./app-views/LiveAppView";
import LiveAppAction from "./app-views/LiveAppAction";
import { installedViewReceipt, resolveLiveAction, resolveLiveView, type AppViewReceipt } from "./app-views/viewReceipt";
import { useChatCollapsed } from "./conversationClient";
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
  bundleDigest: string;
  viewReceipt: AppViewReceipt | null;
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
  const viewReceipt = useMemo(
    () => installation && installation.install_id === installId ? installedViewReceipt(installation) : null,
    [installation, installId],
  );
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
  // CAD-1051: desktop rail. Narrow widths keep the drawer above.
  // CAD-1098: collapse state is per app installation.
  const [chatCollapsed, setChatCollapsed] = useChatCollapsed(installId);
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
    liveView: string | null,
    liveAction: string | null,
  ) => `${installId}|${ctx ?? ""}|${record ?? ""}|${appview ?? ""}|${crm ?? ""}|${liveView ?? ""}|${liveAction ?? ""}`;
  const writeQuery = useCallback(
    (
      patch: {
        ctx?: string | null;
        appview?: OutletView | null;
        record?: string | null;
        crm?: CrmSection | null;
        view?: string | null;
        action?: string | null;
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
        // CAD-1058: `segment` is the New campaign dialog's preselect; it
        // never outlives the dialog (cancel, create or any other view).
        if (patch.appview !== "new") q.delete("segment");
      }
      if (patch.record !== undefined) {
        if (patch.record === null) q.delete("record");
        else q.set("record", patch.record);
      }
      if (patch.crm !== undefined) {
        if (patch.crm === null || patch.crm === "customers") q.delete("crm");
        else q.set("crm", patch.crm);
      }
      if (patch.view !== undefined) {
        if (patch.view === null || patch.view === "") q.delete("view");
        else q.set("view", patch.view);
      } else if (patch.appview !== undefined || patch.record !== undefined || patch.crm !== undefined) {
        q.delete("view");
      }
      if (patch.action !== undefined) {
        if (patch.action === null || patch.action === "") q.delete("action");
        else q.set("action", patch.action);
      } else if (patch.appview !== undefined || patch.record !== undefined || patch.crm !== undefined || patch.view !== undefined) {
        q.delete("action");
      }
      if (patch.clearContractPreview) {
        q.delete("contract-preview");
        q.delete("contract-preview-view");
      }
      handledQuery.current = queryKey(q.get("ctx"), q.get("record"), q.get("appview"), q.get("crm"), q.get("view"), q.get("action"));
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
        bundleDigest: installation.digest,
        viewReceipt,
      });
    } else {
      onInstallation(null);
    }
    return () => onInstallation(null);
  }, [installId, installation, viewReceipt, onInstallation]);

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
    writeQuery({ ctx: null, appview: null, record: null, crm: null, view: null, clearContractPreview: true }, { replace: true });
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
    const urlLiveView = query.get("view");
    const urlLiveAction = query.get("action");
    if (handledQuery.current === queryKey(urlCtx, urlRecord, urlView, urlCrm, urlLiveView, urlLiveAction)) return;
    handledQuery.current = queryKey(urlCtx, urlRecord, urlView, urlCrm, urlLiveView, urlLiveAction);
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
    // Single-company mode: with exactly one active context a scopeless
    // record link is unambiguous, so it resolves to that context.
    const soleContext = urlCtx === null && urlRecord !== null && !badRecord && activeIds.length === 1;
    const scopelessRecord = urlCtx === null && urlRecord !== null && !badRecord && !soleContext;
    if (staleCtx) {
      setContextId(fallbackContext());
      setLinkNotice(
        installation.name === "crm"
          ? "This link names a scope that is not active in this CRM installation, so nothing was opened. Ask an administrator to review the installation's CRM setup."
          : "The linked context is not active in this installation — the selection was cleared.",
      );
    } else if (soleContext) {
      setContextId(activeIds[0]);
      rememberContext(installId, activeIds[0]);
      setLinkNotice(null);
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
    if (badCrm || badRecord || staleCtx || scopelessRecord || soleContext) {
      writeQuery(
        {
          ctx: soleContext ? activeIds[0] : staleCtx ? null : undefined,
          crm: badCrm ? null : undefined,
          record: badRecord || staleCtx || scopelessRecord ? null : undefined,
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
  // The chat's descriptor comes from the installation's own approved package
  // (served pinned to its digest); with none, the pane is plain shared chat.
  const verified = installation !== null && installation.install_id === installId;
  const liveViewRequested = query.has("view");
  const requestedLiveViewId = query.get("view");
  const liveViewResult = useMemo(
    () => liveViewRequested ? resolveLiveView(viewReceipt, requestedLiveViewId, recordId) : null,
    [liveViewRequested, requestedLiveViewId, recordId, viewReceipt],
  );
  const liveActionRequested = query.has("action");
  const actionValues = query.getAll("action");
  const actionScopeValid = query.getAll("ctx").length <= 1;
  const requestedLiveActionId = actionValues.length === 1 && actionScopeValid ? actionValues[0] : null;
  const actionRecordValues = query.getAll("record");
  const actionRecordId = actionRecordValues.length <= 1 ? recordId : "";
  const conflictingOutletRequested = ["view", "appview", "crm", "contract-preview", "contract-preview-view"]
    .some((key) => query.has(key));
  const liveActionResult = useMemo(
    () => liveActionRequested ? resolveLiveAction(viewReceipt, requestedLiveActionId, actionRecordId, conflictingOutletRequested) : null,
    [liveActionRequested, requestedLiveActionId, actionRecordId, viewReceipt, conflictingOutletRequested],
  );
  const liveContextId = binding.scope?.context_id ?? "";
  const liveViewEnabled = viewer.operator && verified && !loading && loadError === null
    && liveContextId !== "" && activeIds.includes(liveContextId);
  const liveViewBlockedReason = !viewer.operator
    ? "Sign in as the operator before reading live app data."
    : loading
      ? "Waiting for the verified installation and context receipts."
      : loadError !== null
        ? loadError
        : !verified
          ? "This route does not match a verified installed app."
          : binding.error ?? (liveContextId === ""
            ? "Choose an existing active app context before reading live records."
            : !activeIds.includes(liveContextId) ? "The selected app context is no longer active." : null);
  const chatDescriptor = useAppChat(
    installId,
    verified ? installation.digest : null,
    verified ? installation.name : null,
  );
  // The shell's own screen ids (D5): the CRM outlet's sections, or the generic
  // outlet's `list` and `new`; the private-screen app owns its own and has none.
  const chatScreen = !verified || isSocial ? null : installation.name === "crm" ? crmSection : view;
  // `open-view` card action: navigate within this installation and context.
  const openView = useCallback(
    (target: string) => {
      if (installation?.name === "crm") {
        if (target === "customers" || target === "segments" || target === "campaigns") {
          writeQuery({ crm: target, appview: null, record: null });
        }
      } else if (target === "list" || target === "new") {
        writeQuery({ appview: target === "list" ? null : target, record: null });
      }
    },
    [installation, writeQuery],
  );

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
        {isDev && installation !== null && !liveViewRequested && !liveActionRequested && (
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

      <div className="app-shell-grid" data-chat-collapsed={chatCollapsed || undefined}>
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
          <Conversation
            mode={{
              kind: "app",
              installId,
              contextId: binding.scope?.context_id ?? "",
              screen: chatScreen,
              recordOpen: recordId !== null,
              contextName: contextLabel(contexts, contextId),
              descriptor: chatDescriptor,
            }}
            density="compact"
            viewer={viewer}
            binding={binding}
            collapsed={chatCollapsed}
            onCollapsed={setChatCollapsed}
            onOpenView={openView}
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
                // The private-screen frame owns its own chrome — the
                // shell shows no installId/version/context subtitle.
                null
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
                <p className="self-start rounded border border-warn/40 px-3 py-1.5 text-label text-warn" role="alert" data-link-notice>
                  {linkNotice}
                </p>
              )}
              {liveActionRequested ? (
                liveActionResult?.ok ? (
                  <LiveAppAction
                    key={`${installId}:${liveContextId}:${liveActionResult.route.action.id}:${liveActionResult.route.recordId ?? ""}:${viewReceipt?.bundleDigest ?? ""}`}
                    installId={installId}
                    contextId={liveContextId}
                    receipt={viewReceipt!}
                    route={liveActionResult.route}
                    enabled={liveViewEnabled}
                    blockedReason={liveViewBlockedReason}
                    returnHref={boundViewHref(href, liveActionResult.route.operation === "create"
                      ? liveActionResult.route.tableView.id : liveActionResult.route.detailViewId,
                    liveActionResult.route.operation === "create" ? null : liveActionResult.route.recordId, liveContextId)}
                    detailHref={(viewId, id) => boundViewHref(href, viewId, id, liveContextId)}
                  />
                ) : (
                  <div className="av" data-live-action-refused>
                    <ErrorNotice>
                      {liveActionResult?.reason ?? "The requested live action is unavailable."}{" "}
                      <Link href={clearLiveViewHref(href)} className="lnk">Return to the installed app</Link>
                    </ErrorNotice>
                  </div>
                )
              ) : liveViewRequested ? (
                liveViewResult?.ok ? (
                  <LiveAppView
                    key={`${installId}:${liveContextId}:${liveViewResult.route.view.id}:${liveViewResult.route.recordId ?? ""}`}
                    installId={installId}
                    contextId={liveContextId}
                    receipt={viewReceipt!}
                    route={liveViewResult.route}
                    enabled={liveViewEnabled}
                    blockedReason={liveViewBlockedReason}
                    returnHref={clearLiveViewHref(href)}
                    backHref={liveViewResult.route.tableView && liveViewResult.route.op === "show"
                      ? boundViewHref(href, liveViewResult.route.tableView.id, null, liveContextId)
                      : null}
                    detailHref={(viewId, id) => boundViewHref(href, viewId, id, liveContextId)}
                    createActionHref={viewReceipt?.actionDescriptor?.actions.some((action) => action.id === "customer.create")
                      && liveViewResult.route.op === "list" && liveViewResult.route.binding.source === "customers"
                      ? boundActionHref(href, "customer.create", null, liveContextId) : null}
                    editActionHref={viewReceipt?.actionDescriptor?.actions.some((action) => action.id === "customer.update")
                      && liveViewResult.route.op === "show" && liveViewResult.route.binding.source === "customers"
                      ? boundActionHref(href, "customer.update", liveViewResult.route.recordId, liveContextId) : null}
                  />
                ) : (
                  <div className="av" data-live-view-refused>
                    <ErrorNotice>
                      {liveViewResult?.reason ?? "The requested live view is unavailable."}{" "}
                      <Link href={clearLiveViewHref(href)} className="lnk">Return to the installed app</Link>
                    </ErrorNotice>
                  </div>
                )
              ) : previewKey !== null ? (
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

export type { ChatBinding, ChatScope };

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
  q.delete("view");
  q.delete("action");
  const s = q.toString();
  return path + (s ? `?${s}` : "");
}

export function boundViewHref(href: string, viewId: string, recordId: string | null, contextId: string): string {
  const [path, search] = href.split("?");
  const q = new URLSearchParams(search ?? "");
  if (contextId) q.set("ctx", contextId);
  q.set("view", viewId);
  if (recordId === null) q.delete("record");
  else q.set("record", recordId);
  q.delete("appview");
  q.delete("crm");
  q.delete("action");
  q.delete("contract-preview");
  q.delete("contract-preview-view");
  const s = q.toString();
  return path + (s ? `?${s}` : "");
}

export function boundActionHref(href: string, actionId: string, recordId: string | null, contextId: string): string {
  const [path, search] = href.split("?");
  const q = new URLSearchParams(search ?? "");
  if (contextId) q.set("ctx", contextId);
  q.set("action", actionId);
  if (recordId === null) q.delete("record");
  else q.set("record", recordId);
  q.delete("view");
  q.delete("crm");
  q.delete("appview");
  q.delete("contract-preview");
  q.delete("contract-preview-view");
  const s = q.toString();
  return path + (s ? `?${s}` : "");
}

function clearLiveViewHref(href: string): string {
  const [path, search] = href.split("?");
  const q = new URLSearchParams(search ?? "");
  q.delete("view");
  q.delete("action");
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
