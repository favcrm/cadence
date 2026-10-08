import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { navigate, useHref } from "../../lib/useLocation";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import type { Viewer } from "../projects/work";
import { workspaceApps, type AppContext, type Installation } from "../workspace-apps/workspaceApps";
import { chatScreenFor } from "./chatScreen";
import { initialContext, rememberedContext, rememberContext, subscribeContext } from "../workspace-apps/contextSelection";
import CrmOutlet, { type CrmSection, type OutletView } from "./CrmOutlet";
import Conversation from "./chat/Conversation";
import { useAppChatProjection } from "./chat/descriptorClient";
import type { ChatBinding, ChatScope } from "./chat/types";
import { assertRecordId, type HostScope } from "./hostActions";
import { isDev } from "../../env";
import AppViewContractPreview, { contractPreviewHref, contractPreviewKey } from "./app-views/AppViewContractPreview";
import { useChatCollapsed, type ConversationLinkRequest } from "./conversationClient";
import "./app-shell.css";

type ParsedConversationLink = Omit<ConversationLinkRequest, "visit"> & { key: string };

function rawQueryValues(rawQuery: string, parameter: string): (string | null)[] {
  const values: (string | null)[] = [];
  for (const part of rawQuery.split("&")) {
    const separator = part.indexOf("=");
    const rawName = separator < 0 ? part : part.slice(0, separator);
    let name: string;
    try {
      name = decodeURIComponent(rawName.replace(/\+/g, " "));
    } catch {
      continue;
    }
    if (name !== parameter) continue;
    const rawValue = separator < 0 ? "" : part.slice(separator + 1);
    try {
      values.push(decodeURIComponent(rawValue.replace(/\+/g, " ")));
    } catch {
      values.push(null);
    }
  }
  return values;
}

/** Read host selectors only. Membership and current scope are checked
 *  separately against the installation's list and verified shell binding. */
function parseConversationLink(href: string): ParsedConversationLink | null {
  const queryStart = href.indexOf("?");
  if (queryStart < 0) return null;
  const rawQuery = href.slice(queryStart + 1).split("#", 1)[0];
  const params = new URLSearchParams(rawQuery);
  const values = params.getAll("conversation");
  if (values.length === 0) return null;
  const rawValues = rawQueryValues(rawQuery, "conversation");
  const contextValues = params.getAll("ctx");
  const rawContexts = rawQueryValues(rawQuery, "ctx");
  let contextTarget: string | null = null;
  let contextError: string | null = null;
  if (contextValues.length > 1 || rawContexts.length > 1) {
    contextError = "This conversation link names more than one context — nothing was opened.";
  } else if (contextValues.length === 1 && (rawContexts.length !== 1 || rawContexts[0] === null)) {
    contextError = "This conversation link has a malformed context — nothing was opened.";
  } else if (contextValues.length === 1 && contextValues[0] === "") {
    contextError = "This conversation link does not name an active context — nothing was opened.";
  } else if (contextValues.length === 1) {
    contextTarget = contextValues[0];
  }
  const invalid = (error: string): ParsedConversationLink => ({
    key: JSON.stringify(["invalid", values, rawValues, contextTarget, contextError, error]),
    target: null,
    error,
    contextTarget,
    contextError,
  });
  if (values.length > 1 || rawValues.length > 1) {
    return invalid("This link names more than one conversation — nothing was opened.");
  }
  if (values.length !== 1 || rawValues.length !== 1 || rawValues[0] === null) {
    return invalid("This link has a malformed conversation id — nothing was opened.");
  }
  if (values[0] === "") {
    return invalid("This link does not name a conversation — nothing was opened.");
  }
  return {
    key: JSON.stringify(["target", values[0], contextTarget, contextError]),
    target: values[0],
    error: null,
    contextTarget,
    contextError,
  };
}

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
  const parsedConversationLink = useMemo(() => parseConversationLink(href), [href]);
  const conversationVisit = useRef(0);
  const conversationRequestRef = useRef<{ key: string | null; request: ConversationLinkRequest | null }>({
    key: null,
    request: null,
  });
  const requestKey = parsedConversationLink === null
    ? null
    : JSON.stringify([installId, parsedConversationLink.key]);
  if (conversationRequestRef.current.key !== requestKey) {
    conversationRequestRef.current = {
      key: requestKey,
      request: parsedConversationLink === null
        ? null
        : {
            visit: ++conversationVisit.current,
            target: parsedConversationLink.target,
            error: parsedConversationLink.error,
            contextTarget: parsedConversationLink.contextTarget,
            contextError: parsedConversationLink.contextError,
          },
    };
  }
  const conversationRequest = conversationRequestRef.current.request;
  const [installation, setInstallation] = useState<Installation | null>(null);
  const [contexts, setContexts] = useState<AppContext[]>([]);
  const [loadFailure, setLoadFailure] = useState<{ key: string; message: string } | null>(null);
  const [requestLoading, setRequestLoading] = useState(true);
  const [completedReceiptKey, setCompletedReceiptKey] = useState<string | null>(null);
  const [contextId, setContextId] = useState("");
  const [linkNotice, setLinkNotice] = useState<string | null>(null);
  // Social-content's picker owns its selection; the shell observes it
  // in the same tab so chat sends carry the current scope, never the
  // one from the shell's last render.
  const [socialContext, setSocialContext] = useState<string | null>(null);
  const [socialContextInstall, setSocialContextInstall] = useState<string | null>(null);
  // Context receipts are per operator/install/selection and per scoped
  // conversation visit. The browser selection only invalidates a receipt;
  // membership still comes exclusively from the fresh server response.
  const scopedConversationVisit =
    conversationRequest !== null &&
    conversationRequest.target !== null &&
    conversationRequest.contextTarget !== null
      ? conversationRequest.visit
      : null;
  const readInputKey = JSON.stringify([
    installId,
    viewer.operator,
    socialContextInstall === installId,
    socialContextInstall,
    socialContextInstall === installId ? socialContext : null,
    scopedConversationVisit,
  ]);
  const readVersionRef = useRef({ inputKey: "", version: 0 });
  if (readVersionRef.current.inputKey !== readInputKey) {
    readVersionRef.current = {
      inputKey: readInputKey,
      version: readVersionRef.current.version + 1,
    };
  }
  const receiptKey = JSON.stringify([readInputKey, readVersionRef.current.version]);
  const currentReceiptKeyRef = useRef(receiptKey);
  currentReceiptKeyRef.current = receiptKey;
  const currentReadComplete = completedReceiptKey === receiptKey;
  const loadError = loadFailure?.key === receiptKey ? loadFailure.message : null;
  const socialSelectionReady =
    installation?.name !== "social-content" || socialContextInstall === installId;
  const loading = viewer.operator
    ? requestLoading || !currentReadComplete || !socialSelectionReady
    : requestLoading;
  const receiptReady =
    viewer.operator &&
    currentReadComplete &&
    loadError === null &&
    socialSelectionReady &&
    installation?.install_id === installId;
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
  // Narrow widths: one full-width panel at a time (the approved mock's
  // Assistant | Workspace switch). Both stay mounted — only CSS hides the
  // other — so draft, stream and workspace state survive switching.
  const [panel, setPanel] = useState<"chat" | "work">("chat");
  // CAD-1051: desktop rail. Narrow widths keep the drawer above.
  // CAD-1098: collapse state is per app installation.
  const [chatCollapsed, setChatCollapsed] = useChatCollapsed(installId);
  // CAD-861: the app-views/v1 contract preview is a dev-only overlay
  // keyed by `contract-preview` in the URL — it never mounts in a
  // production bundle and never replaces the trusted outlet by default.
  const previewKey = isDev ? contractPreviewKey(query) : null;
  // Installation switches reset outlet state but keep the chat: the
  // first mount preserves direct links, later switches strip them.
  const firstInstall = useRef(installId);
  const handledQuery = useRef<string | undefined>(undefined);
  const [adoptedContextKey, setAdoptedContextKey] = useState<string | null>(null);

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
        conversation?: string | null;
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
        // CAD-1058: `segment` is the New campaign dialog's preselect; it
        // never outlives the dialog (cancel, create or any other view).
        if (patch.appview !== "new") q.delete("segment");
      }
      if (patch.record !== undefined) {
        if (patch.record === null) q.delete("record");
        else q.set("record", patch.record);
      }
      if (patch.conversation !== undefined) {
        if (patch.conversation === null) q.delete("conversation");
        else q.set("conversation", patch.conversation);
      }
      if (patch.crm !== undefined) {
        if (patch.crm === null || patch.crm === "customers") q.delete("crm");
        else q.set("crm", patch.crm);
      }
      if (patch.clearContractPreview) {
        q.delete("contract-preview");
        q.delete("contract-preview-view");
      }
      const adoptedKey = queryKey(q.get("ctx"), q.get("record"), q.get("appview"), q.get("crm"));
      handledQuery.current = adoptedKey;
      setAdoptedContextKey(adoptedKey);
      const s = q.toString();
      navigate(path + (s ? `?${s}` : ""), { replace: opts?.replace });
    },
    [href, installId],
  );

  // Verified installation/context receipts. A current key mismatch is
  // pending synchronously (before this effect runs); stale requests cannot
  // publish data or settle loading after selection/install/unmount changes.
  useEffect(() => {
    let current = true;
    const controller = new AbortController();
    const isCurrent = () =>
      current &&
      !controller.signal.aborted &&
      currentReceiptKeyRef.current === receiptKey;
    // CAD-1193: `null` (still checking/unavailable) is not a refusal —
    // wait for the resolved role rather than painting a sign-in, and
    // never issue the protected detail/context reads before
    // `operator === true`.
    if (viewer.operator !== true) {
      setRequestLoading(false);
      return () => {
        current = false;
        controller.abort();
      };
    }
    setRequestLoading(true);
    Promise.all([
      workspaceApps.detail(installId, controller.signal),
      workspaceApps.contexts(installId, controller.signal),
    ])
      .then(([next, nextContexts]) => {
        if (!isCurrent()) return;
        setInstallation(next);
        setContexts(nextContexts);
        // CAD-1174: a CRM installation without a scope is unusable — the
        // record lists, the segments and the email sender all need one,
        // and asking the operator to invent a scope is the friction this
        // removes. Create the single `General` scope on first load;
        // `request_id` is stable, so a second load or a race resolves to
        // the same row instead of minting another. Apps that run without
        // a brand context by design (Social) are untouched.
        if (next.name === "crm" && !nextContexts.some((value) => value.state === "active")) {
          workspaceApps
            .createContext(installId, {
              label: "General",
              input_defaults: {},
              request_id: `general-${installId}`,
            })
            .then((created) => {
              // A conforming daemon always answers with the row (a refusal
              // rejects); anything else — a method-blind mock, a proxy
              // error page parsed as JSON — must never reach the render's
              // `value.state` filter, so validate the shape, not just
              // truthiness.
              const row =
                created && typeof created.id === "string" && typeof created.state === "string"
                  ? created
                  : null;
              if (!controller.signal.aborted && row !== null) {
                setContexts((previous) => [...previous, row]);
              }
            })
            .catch(() => {
              // The scope-entry notice below already tells the operator
              // what to do; a failed default never blocks the app.
            });
        }
        setLoadFailure(null);
        setCompletedReceiptKey(receiptKey);
      })
      .catch((e: unknown) => {
        if (isCurrent()) {
          setLoadFailure({
            key: receiptKey,
            message: e instanceof Error ? e.message : "Could not load this app",
          });
          setCompletedReceiptKey(receiptKey);
        }
      })
      .finally(() => {
        if (isCurrent()) setRequestLoading(false);
      });
    return () => {
      current = false;
      controller.abort();
    };
  }, [installId, receiptKey, viewer.operator]);

  // Board-level App menu identity: report the verified receipt (or
  // null while it is loading, failed, or belongs to another install)
  // so the shared sidebar/phone menu can nest this installation's
  // sections. Clearing on cleanup keeps a departed installation from
  // lingering in host navigation.
  useEffect(() => {
    if (!onInstallation) return;
    if (receiptReady && installation && installation.install_id === installId) {
      onInstallation({
        installId,
        kind: installation.name,
        title: installation.title || installation.name,
      });
    } else {
      onInstallation(null);
    }
    return () => onInstallation(null);
  }, [installId, installation, onInstallation, receiptReady]);

  useEffect(() => {
    setSocialContext(rememberedContext(installId));
    setSocialContextInstall(installId);
    return subscribeContext((changed, next) => {
      if (changed === installId) {
        setSocialContext(next);
        setSocialContextInstall(installId);
      }
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
    setLoadFailure(null);
    setLinkNotice(null);
    setContextId("");
    writeQuery({ ctx: null, appview: null, record: null, conversation: null, crm: null, clearContractPreview: true }, { replace: true });
    // The strip marks the emptied query handled: unmark so adoption
    // still runs once the new installation's contexts load.
    handledQuery.current = undefined;
    setAdoptedContextKey(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [installId]);

  const activeContexts = useMemo(
    () => (receiptReady ? contexts.filter((c) => c.state === "active") : []),
    [contexts, receiptReady],
  );
  const activeIds = useMemo(() => activeContexts.map((c) => c.id), [activeContexts]);

  // Adopt the URL's context once contexts load, and on later external
  // URL changes (browser back). A linked context must be active here;
  // a stale, inactive or ambiguous link clears the selection with a
  // notice instead of guessing. Malformed record ids are stripped.
  useEffect(() => {
    // The loaded receipts must belong to this installation: on a
    // switch commit the state still holds the previous install while
    // the URL already names the next one — adopting there would clear
    // or poison the wrong scope.
    if (!receiptReady || loading || installation === null || installation.install_id !== installId) return;
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
    const incomingQueryKey = queryKey(urlCtx, urlRecord, urlView, urlCrm);
    if (handledQuery.current === incomingQueryKey) {
      setAdoptedContextKey(incomingQueryKey);
      return;
    }
    handledQuery.current = incomingQueryKey;
    setAdoptedContextKey(incomingQueryKey);
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
    const hasConversationLink = query.getAll("conversation").length > 0;
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
          // Preserve a stale ctx selector alongside a conversation link so
          // its target can fail visibly instead of becoming contextless.
          ctx: soleContext ? activeIds[0] : staleCtx && !hasConversationLink ? null : undefined,
          crm: badCrm ? null : undefined,
          record: badRecord || staleCtx || scopelessRecord ? null : undefined,
          appview: staleCtx ? null : undefined,
        },
        { replace: true },
      );
    }
  }, [loading, receiptReady, installation, activeIds, query, installId, writeQuery]);

  // The default selection, without persisting an empty choice when
  // this installation has no active contexts to choose from.
  const fallbackContext = () =>
    activeIds.length > 0 ? initialContext(installId, activeIds) : "";

  // The context picker is removed (operator review): context follows
  // the URL, not a selector.

  // The App binding for chat sends: install plus the concrete context
  // the shell owns (generic outlet) or the workspace screen owns
  // (social, observed live via subscription). Empty context sends
  // plain chat — there is no App scope to bind. A selected context
  // that is no longer active blocks the send early with a clear
  // message; the server re-proves every binding on send regardless.
  const verified = receiptReady && installation !== null && installation.install_id === installId;
  const wantedContext = installation?.name === "social-content" ? socialContext || "" : contextId;
  const bindingUnavailableError = loadError ??
    (!viewer.operator
      ? "Sign in as the operator to verify this app context before sending."
      : loading
        ? "The current app context is still being verified. Wait before sending."
        : "The current app context could not be verified. Retry before sending.");
  const binding: ChatBinding = verified
    ? chatBinding({ installId, wanted: wantedContext, known: true, activeIds })
    : { scope: null, error: bindingUnavailableError };
  const contextAdoptionReady = installation?.name === "social-content"
    ? socialContextInstall === installId
    : adoptedContextKey === queryKey(query.get("ctx"), query.get("record"), query.get("appview"), query.get("crm"));
  const contextProofStatus = loadError !== null || (!viewer.operator && !loading)
    ? "failed"
    : loading || !verified || !contextAdoptionReady
      ? "pending"
      : "ready";
  const conversationContextProof = {
    status: contextProofStatus as "pending" | "ready" | "failed",
    contextId: binding.scope?.context_id ?? null,
    error: contextProofStatus === "failed"
      ? loadError ?? "The current app context could not be verified."
      : binding.error ?? (binding.scope === null ? "No active app context is available." : null),
  };
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
  // CAD-1174: with a single active scope the context concept is not a
  // choice — the operator never sees it. `General` is the one scope a
  // CRM installation gets (created below when it has none); every
  // other app keeps the surfaces it has today.
  const singleScope = verified && installation.name === "crm" && activeContexts.length <= 1;
  const chatProjection = useAppChatProjection(
    installId,
    verified ? installation.digest : null,
    verified ? installation.name : null,
  );
  const chatDescriptor = chatProjection.projection?.chat ?? null;
  // The shell's own screen ids (D5): the CRM outlet's sections, or the generic
  // outlet's `list` and `new`; the private-screen app owns its own and has none.
  const chatScreen = chatScreenFor(installation, verified, crmSection, view);
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
        {isSocial && activeIds.length > 1 && (
          // A private screen offers no in-outlet picker (the screen is the
          // outlet's first child). With 2+ active contexts the choice lives
          // here in the header row; 0 or 1 never shows a control.
          <select
            className="app-shell-context-pick"
            aria-label="Context"
            data-social-context
            value={socialContext !== null && activeIds.includes(socialContext) ? socialContext : ""}
            onChange={(e) => e.target.value !== "" && rememberContext(installId, e.target.value)}
          >
            <option value="" disabled>
              Choose a context
            </option>
            {contexts
              .filter((c) => c.state === "active")
              .map((c) => (
                <option key={c.id} value={c.id}>
                  {c.config.label}
                </option>
              ))}
          </select>
        )}
        <Link href="/" className="lnk text-micro app-shell-home-link" data-chat-home-link>
          Earlier history is in Home
        </Link>
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
      </div>

      <div className="app-shell-panels" role="group" aria-label="Panel">
        {(["chat", "work"] as const).map((p) => (
          <button
            key={p}
            type="button"
            className="app-shell-panel-tab"
            aria-pressed={panel === p}
            aria-controls={p === "chat" ? "app-shell-chat" : "app-shell-workspace"}
            onClick={() => setPanel(p)}
          >
            {p === "chat" ? "Assistant" : "Workspace"}
          </button>
        ))}
      </div>

      <div className="app-shell-grid" data-chat-collapsed={chatCollapsed || undefined} data-shell-panel={panel}>
        <div id="app-shell-chat" className="app-shell-chat" aria-label="Assistant chat">
          <Conversation
            mode={{
              kind: "app",
              installId,
              contextId: binding.scope?.context_id ?? "",
              screen: chatScreen,
              recordOpen: recordId !== null,
              contextName: singleScope ? null : contextLabel(activeContexts, contextId),
              scopeLabel: (singleScope ? null : contextLabel(activeContexts, contextId)) || title,
              descriptor: chatDescriptor,
              promptLayout: isSocial ? "list" : "chips",
              conversationRequest,
              contextProof: conversationContextProof,
              fileUpload: chatProjection.projection?.fileUpload ?? { declared: false, available: false },
              fileUploadLoading: chatProjection.loading || !verified || loading || loadError !== null,
            }}
            density="compact"
            viewer={viewer}
            binding={binding}
            onFileUploadUnavailable={() => void chatProjection.refresh()}
            collapsed={chatCollapsed}
            onCollapsed={setChatCollapsed}
            onOpenView={openView}
          />
        </div>
        <section id="app-shell-workspace" className="app-shell-outlet" aria-label={`${title} workspace`}>
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
          {!loading && !installation && !loadError && viewer.operator === null && (
            <main className="card px-4 py-5" aria-label="App" role="status">
              <h2 className="text-cardtitle font-medium text-ink-100">
                {title}
              </h2>
              {viewer.access === "unavailable" ? (
                // CAD-1193: a completed check that could not answer is
                // unavailable, not still checking and not a sign-out —
                // the retry re-runs the real metadata probe, never a
                // reload and never a login.
                <p className="text-secondary text-ink-400 mt-1">
                  Access could not be confirmed — the board's access
                  check did not answer. Write controls stay off and no
                  records load until it does.{" "}
                  {viewer.onRetryAccess && (
                    <Button size="sm" onClick={viewer.onRetryAccess}>
                      Retry access check
                    </Button>
                  )}
                </p>
              ) : (
                <p className="text-secondary text-ink-400 mt-1">
                  Checking whether this session may inspect this
                  installation… If access was denied, the board will say so
                  here.
                </p>
              )}
            </main>
          )}
          {!loading && !installation && !loadError && viewer.operator === false && (
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
                  {activeContexts.find((c) => c.id === contextId)?.config.label ?? "No context"} · {installation.version}
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
                    {activeContexts.map((c) => (
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
  const [path, search] = href.split("?");
  const q = new URLSearchParams(search ?? "");
  const currentContexts = q.getAll("ctx");
  if (
    contextId === boundId &&
    (currentContexts.length === 0 || (currentContexts.length === 1 && currentContexts[0] === boundId))
  ) {
    return href;
  }
  q.set("ctx", contextId);
  q.delete("record");
  q.delete("appview");
  // An explicit company switch supersedes a pending conversation deep link.
  q.delete("conversation");
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
