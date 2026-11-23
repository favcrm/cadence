import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useQuery, useResource } from "../../lib/useResource";
import { useTranslate } from "../../lib/locale";
import { onThreadReload } from "../../lib/masterChanged";
import { displayUiNotice } from "../../lib/uiNotice";
import { subscribeSse } from "../../lib/sse";
import type { MasterState, ThreadRef } from "../../lib/types";
import { agentOn } from "../../lib/agentOn";
import type { HostedTemplateList, HostedTemplateRail } from "../templates/HostedTemplateReview";
import SharedComposer from "../app-shell/chat/Composer";
import { composerDraftState, requestComposerFocus, setComposerDraft } from "../app-shell/chat/composerStore";
import Conversation from "../app-shell/chat/Conversation";
import { appendComposerDraft } from "./composerFill";
import { tailTop } from "./dock";
import { WORKSPACE_SKILLS_COMMAND } from "./master";
import NeedsRail, { type DocumentFollowUpState } from "./NeedsRail";
import { homeDecisions } from "../decisions/useDecisions";
import { StagedSourceContext } from "../../ui/StagedChip";
import { useDockHeight } from "./useDockHeight";
import {
  applyMemberEarlier,
  fetchMemberEarlier,
  memberHistory,
  memberThreadStore,
  memberWrite,
  sendToAssistant,
} from "./memberThread";
import { initialFollow, motionBehavior, onSend, type Follow } from "./scrollFollow";
import { StreamBanner, ThreadTurn } from "./ThreadView";
import {
  inFlightTurnKey,
  itemMessage,
  lastSeq,
  threadItems,
  visibleWindow,
  WINDOW,
} from "./thread";
import { ImageConversation } from "../canvas/imageConversation";
import { useCanvasFeed } from "../canvas/feed";
import { BrowserCard } from "../canvas/BrowserPanel";
import { useBrowserFeed } from "../canvas/browserStore";
import {
  AssistantAboutButton,
  AssistantActorProvider,
  AssistantBootstrapCard,
  AssistantFace,
  postPersonalProfile,
  toPersonalCompleteMutation,
  toPersonalUpdateMutation,
  type AssistantActorValue,
  type PersonalProfileSaveChange,
  type PersonalProfileSaveResult,
} from "../assistant/AssistantIdentity";
import {
  displayName,
  messageLabel,
  messagePlaceholder,
  personalAssistantProfileHref,
  readPersonalIdentity,
  scopeDescriptor,
  type AvatarLook,
  type IdentityRead,
  type PersonalProfileMutation,
  type PersonalProfileOutcome,
} from "../assistant/identity";

/**
 * CAD-1485: the member's own private-assistant chat, rendered inside the
 * existing Home screen in place of the Master workspace. The public
 * session cookie is the only credential; the board mints and verifies
 * the assistant envelope server-side, so this component holds no member
 * authority, no envelope and no signed SSO fragment — just the `sub`
 * and session the verified session already carries.
 *
 * History stays on the member's paged route. The bound conversation's
 * stream reports activity, not transcript content. Sends land an optimistic
 * row reconciled by `message` id. The store keys on the session-scoped
 * `scope`, so a member switch and a re-login never cross, and the Master
 * draft is never reused.
 * `key={scope}` on this component remounts it on any scope change, so
 * no local state, follow ref or scroll position leaks across.
 */
/** The private surface's one state read: binding alias, conversation,
 *  bootstrap phase and the committed identity projection. */
interface MemberSurface {
  ok: boolean;
  bootstrap: string | null;
  conversation: string | null;
  alias: string;
  identity: IdentityRead;
}

const LOADING_IDENTITY: IdentityRead = { state: "loading" };


export default function MemberChat({
  scope,
  readOnly,
  onOpenIssue,
  decisions = false,
  documentFollowUps,
  templateRail,
  templateCount,
  templateList,
}: {
  /** The session-scoped store/draft key — `${sessionId}:${sub}`. */
  scope: string;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
  decisions?: boolean;
  documentFollowUps?: DocumentFollowUpState;
  templateRail?: HostedTemplateRail;
  templateCount?: number;
  templateList?: HostedTemplateList;
}) {
  const t = useTranslate();
  const roster = useQuery(resources.agents);
  const store = memberThreadStore(scope);
  const thread = useResource(store);
  // The member scope is never named in stream-resource invalidations
  // (`memberThread` is not in RESOURCE_NAMES) and `useResource` only
  // subscribes — so nothing else starts the first `loadThread`. This
  // mount revalidate is the same stale-while-revalidate entry the
  // Master thread's `useQuery` uses; without it the store stays
  // `loading` forever and no history truth can ever record.
  useEffect(() => {
    void store.revalidate();
  }, [store]);
  // AOS-711: a share request card was decided; read the newest page again so the rewritten card and its file show.
  useEffect(
    () =>
      onThreadReload(() => {
        if (store.get().data) memberWrite(store, (cur) => ({ ...cur!, tailReload: true }));
        void store.invalidate();
      }),
    [store],
  );
  const [surface, setSurface] = useState<MemberSurface | null>(null);
  const [liveTurn, setLiveTurn] = useState<{
    conversation: string;
    message: string | null;
    state: "working" | "reconnecting" | "stopped";
  } | null>(null);
  const retryActivity = useRef<(() => void) | null>(null);
  const [pendingLook, setPendingLook] = useState<AvatarLook | null>(null);
  const alive = useRef(true);
  const identityGate = useRef<{ inflight: boolean; again: boolean; timer: number | null }>({
    inflight: false,
    again: false,
    timer: null,
  });
  const loadSurfaceRef = useRef<() => void>(() => {});
  const [limit, setLimit] = useState(WINDOW);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  const [sendNote, setSendNote] = useState<string | null>(null);
  const follow = useRef<Follow>(initialFollow());
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const liveAfter = useRef<number | null>(null);

  const items = useMemo(() => threadItems(thread.data), [thread.data]);
  // History truth is carried in the member payload (round 3): a
  // `Resource.write` stamps ok/error=null/asOf on every optimistic
  // mutation, so `data !== null`/`status` can never prove a read
  // succeeded. `memberHistory` reads the payload's loader-stamped
  // `history` metadata plus the Resource's own retained `error` — a
  // genuine accepted refresh is the only thing that flips `loaded`.
  const history = memberHistory(thread);
  const loaded = history.loaded;
  // The seq watermark for the enter animation: set before the first loaded
  // render paints its rows, so history stays still. A later first message in
  // an empty thread still rises in.
  if (liveAfter.current === null && loaded) {
    liveAfter.current = lastSeq(thread.data) ?? 0;
  }
  const moreBefore = thread.data?.moreBefore === true;

  // The assistant's surface state — the bound conversation, whether
  // BOOTSTRAP onboarding is still pending, the stable alias and the
  // committed identity. One read at a time: a scheduled refresh waits
  // behind an in-flight read instead of racing it, and this component is
  // keyed by scope, so an answer from another session/user cannot land.
  const loadSurface = useCallback(() => {
    const gate = identityGate.current;
    if (gate.inflight) {
      gate.again = true;
      return;
    }
    gate.inflight = true;
    api
      .memberAssistState()
      .then((next) => {
        if (!alive.current) return;
        const identity = readPersonalIdentity(next);
        setSurface({
          ok: true,
          bootstrap: next.assistant.bootstrap_state ?? null,
          conversation: next.conversation_id,
          alias: next.assistant.alias,
          identity,
        });
        // The picker's request clears only when the committed identity
        // actually carries the chosen look.
        setPendingLook((requested) =>
          requested !== null && identity.state === "ready" && identity.identity.look === requested
            ? null
            : requested,
        );
      })
      .catch(() => {
        if (!alive.current) return;
        setSurface((prev) =>
          prev === null
            ? { ok: false, bootstrap: null, conversation: null, alias: "", identity: { state: "unavailable" } }
            : { ...prev, ok: false, identity: { state: "unavailable" } },
        );
      })
      .finally(() => {
        gate.inflight = false;
        if (gate.again && alive.current) {
          gate.again = false;
          loadSurfaceRef.current();
        }
      });
  }, []);
  loadSurfaceRef.current = loadSurface;

  const scheduleSurface = useCallback((delay = 0) => {
    const gate = identityGate.current;
    if (gate.timer !== null) window.clearTimeout(gate.timer);
    gate.timer = window.setTimeout(() => {
      gate.timer = null;
      loadSurfaceRef.current();
    }, delay);
  }, []);

  useEffect(() => {
    alive.current = true;
    scheduleSurface(0);
    return () => {
      alive.current = false;
      const gate = identityGate.current;
      if (gate.timer !== null) window.clearTimeout(gate.timer);
      gate.timer = null;
      gate.again = false;
    };
  }, [scheduleSurface]);

  // Accepted history activity — a reply or completed turn landed — is
  // when a committed profile change can become visible. Refresh then,
  // coalesced behind the single-flight read; no second interval starts.
  const identitySeq = lastSeq(thread.data);
  const seenIdentitySeq = useRef<number | null | undefined>(undefined);
  useEffect(() => {
    if (!loaded) return;
    if (seenIdentitySeq.current === undefined) {
      seenIdentitySeq.current = identitySeq;
      return;
    }
    if (identitySeq !== null && identitySeq !== seenIdentitySeq.current) {
      seenIdentitySeq.current = identitySeq;
      scheduleSurface(600);
    }
  }, [loaded, identitySeq, scheduleSurface]);

  // Paged history is authoritative even if the private stream is unavailable.
  useEffect(() => {
    if (!loaded) return;
    const timer = setInterval(() => void store.refresh(), 6000);
    return () => clearInterval(timer);
  }, [loaded, store]);

  // CAD-2103: this surface does not feed the canvas by itself, so the
  // assistant's conversation (the same id the image provider below uses) is
  // wired to the canvas and the Browser tab explicitly.
  const conversationId = surface?.conversation ?? null;
  useEffect(() => {
    if (conversationId === null || surface?.ok !== true) return;
    // Null names a verified but unnamed turn. Undefined means no verified turn.
    let previous: string | null | undefined;
    const publish = (message?: string | null, confirmed = true) => {
      if (previous !== message) {
        previous = message;
        void store.invalidate();
      }
      setLiveTurn((turn) => {
        if (message === undefined) return confirmed || turn?.state === "working" ? null : turn;
        if (turn?.conversation === conversationId && turn.message === message && turn.state === "working") return turn;
        return { conversation: conversationId, message, state: "working" };
      });
    };
    const interrupt = (state: "reconnecting" | "stopped") => {
      setLiveTurn((turn) =>
        turn?.conversation === conversationId && turn.state !== state ? { ...turn, state } : turn,
      );
    };
    const connect = (lastEventId?: string | null) => subscribeSse({
      url: `/api/threads/master/stream?${new URLSearchParams({ conversation: conversationId })}`,
      // Entry ids advance the reconnect cursor, but member history keeps
      // its own opaque cursor. Don't merge entries or subscribe to previews.
      lastEventId,
      events: ["master", "entry"],
      onEvent: ({ type, data }) => {
        if (type !== "master") return;
        try {
          const snapshot = JSON.parse(data) as MasterState | null;
          if (snapshot?.session?.id === conversationId &&
              (!Object.hasOwn(snapshot, "actor_identity") || snapshot.actor_identity?.conversation_id === conversationId)) {
            const turn = snapshot.turn;
            if (turn?.conversation === conversationId && turn.state === "working") {
              publish(typeof turn.message === "string" && turn.message !== "" ? turn.message : null);
            } else {
              publish();
            }
            return;
          }
        } catch {
          // An unusable frame is not evidence that the agent is working.
        }
        // Don't resume on an unusable frame. Keep the notice until a
        // conversation snapshot or the saved result can settle it.
        publish(undefined, false);
      },
      onError: (state) => interrupt(state === "stopped" ? "stopped" : "reconnecting"),
    });
    let stream = connect();
    retryActivity.current = () => {
      const after = stream.lastEventId();
      stream.close();
      interrupt("reconnecting");
      stream = connect(after);
    };
    return () => {
      retryActivity.current = null;
      stream.close();
      setLiveTurn(null);
    };
  }, [conversationId, store, surface?.ok]);
  // Auto-open is off here: an artifact opens the drawer when the person asks,
  // so on a phone the drawer never covers the chat they are reading.
  useCanvasFeed({ conversation: conversationId, surface: "home", composerKey: `member-assist:${scope}`, items, loaded, running: null, autoOpen: false });
  // Browser activity still follows the last stored item when the feed is down.
  const tail = [...items].reverse().find((i) => !["system", "artifact", "image", "video", "hire", "share"].includes(i.type));
  const turnOpen = tail !== undefined && tail.type !== "answer" && !(tail.type === "pending" && tail.pending.state === "failed");
  useBrowserFeed(conversationId, turnOpen, `${lastSeq(thread.data) ?? 0}:${thread.data?.pending?.length ?? 0}`);

  const [glideTick, setGlide] = useState(0);
  const scrollPanel = useCallback((kind: "glide" | "jump") => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTo({ top: tailTop(el.scrollHeight, el.clientHeight), behavior: motionBehavior(false, kind) });
  }, []);
  // The dock grows with its note, and the thread's bottom padding follows it.
  const dockRef = useRef<HTMLDivElement | null>(null);
  const keepDockTail = useCallback(
    (onTail: boolean) => {
      // Nothing unpins `follow` here, so only the reader's position counts:
      // someone reading history stays where they are while they type.
      if (onTail) scrollPanel("jump");
    },
    [scrollPanel],
  );
  useDockHeight(dockRef, scrollRef, keepDockTail);
  const engage = useCallback(() => {
    const next = onSend(follow.current);
    follow.current = next.state;
    setGlide((n) => n + 1);
  }, []);
  useEffect(() => {
    if (glideTick === 0) return;
    scrollPanel("glide");
  }, [glideTick, scrollPanel]);

  // Reveal held rows first, then continue with the server's opaque
  // backward cursor without changing the forward continuation.
  const onEarlier = useCallback(() => {
    const data = store.get().data;
    if (!data) return;
    const hidden = Math.max(0, threadItems(data).length - limit);
    if (hidden > 0) {
      setLimit((l) => l + WINDOW);
      return;
    }
    const page = fetchMemberEarlier(data);
    if (!page) return;
    setLoadingEarlier(true);
    void page
      .then((older) => {
        memberWrite(store, (s) => applyMemberEarlier(s ?? data, older));
        setLimit((l) => l + WINDOW);
      })
      .catch(() => undefined)
      .finally(() => setLoadingEarlier(false));
  }, [store, limit]);

  const onRetry = useCallback(
    (message: string, text: string, _refs?: ThreadRef[], _attachments?: { id: string }[]) => {
      void sendToAssistant(scope, text, message).then(() => engage());
    },
    [scope, engage],
  );
  const onDiscard = useCallback(
    (message: string) => memberWrite(store, (s) => ({ ...(s ?? { exists: true, entries: [], pending: [] }), pending: (s?.pending ?? []).filter((p) => p.message !== message) })),
    [store],
  );

  const identity = surface?.identity ?? LOADING_IDENTITY;
  const [resumingBootstrap, setResumingBootstrap] = useState(false);

  const runProfileMutation = useCallback(
    async (req: PersonalProfileMutation): Promise<PersonalProfileOutcome> => {
      const outcome = await postPersonalProfile(req);
      if (outcome.ok) {
        setSurface((prev) => ({
          ok: true,
          bootstrap: outcome.bootstrap,
          conversation: outcome.conversation ?? prev?.conversation ?? null,
          alias: outcome.alias,
          identity: { state: "ready", identity: outcome.identity },
        }));
      } else if (outcome.conflict) {
        scheduleSurface(0);
      }
      return outcome;
    },
    [scheduleSurface],
  );

  const saveProfile = useCallback(
    async (change: PersonalProfileSaveChange): Promise<PersonalProfileSaveResult> => {
      const outcome = await runProfileMutation(toPersonalUpdateMutation(change));
      if (!outcome.ok) return outcome;
      return { ok: true, identity: outcome.identity };
    },
    [runProfileMutation],
  );

  const completeBootstrap = useCallback(
    (input: { expected_revision: number; name?: string; look: AvatarLook }) =>
      runProfileMutation(toPersonalCompleteMutation(input)),
    [runProfileMutation],
  );

  const deferBootstrap = useCallback(
    (expected_revision: number) => runProfileMutation({ expected_revision, action: "defer" }),
    [runProfileMutation],
  );

  const resumeBootstrap = useCallback(async () => {
    if (identity.state !== "ready" || resumingBootstrap) return;
    setResumingBootstrap(true);
    await runProfileMutation({ expected_revision: Number(identity.identity.revision), action: "resume" });
    setResumingBootstrap(false);
  }, [identity, resumingBootstrap, runProfileMutation]);

  // CAD-1817: an avatar pick fallback through the existing private send
  // when a board predates `POST /api/member-assistant/profile`.
  const requestLook = useCallback(
    async (look: AvatarLook) => {
      if (identity.state === "ready" && identity.identity.look === look) return { ok: true };
      try {
        const receipt = await sendToAssistant(scope, `Use the ${look} avatar for yourself.`);
        if (!receipt.ok) return { ok: false, error: receipt.error };
        setPendingLook(look);
        engage();
        scheduleSurface(4000);
        return { ok: true };
      } catch (error) {
        return { ok: false, error: error instanceof Error ? error.message : String(error) };
      }
    },
    [identity, scope, engage, scheduleSurface],
  );

  const actor = useMemo<AssistantActorValue>(
    () => ({
      scope: "personal",
      alias: surface?.alias ?? "",
      read: identity,
      // A read-only board cannot mutate or send, so it offers no editor.
      personal: readOnly ? null : { requestLook, pendingLook, saveProfile },
      workspace: null,
    }),
    [surface?.alias, identity, requestLook, pendingLook, saveProfile, readOnly],
  );

  const { shown, hidden } = useMemo(() => visibleWindow(items, limit), [items, limit]);
  const activity = surface?.ok && liveTurn?.conversation === conversationId ? liveTurn : null;
  const sendIndex = activity === null ? -1 : shown.findLastIndex((item) =>
    item.type === "operator"
      ? activity.message === null || itemMessage(item) === activity.message
      : item.type === "pending" && item.pending.state !== "failed" &&
        activity.message !== null && item.pending.message === activity.message,
  );
  // A stored result wins over an older working frame. An unnamed turn can
  // only follow a stored send, never the latest optimistic message.
  const activeItem = sendIndex < 0 || shown.some((item, index) =>
    index > sendIndex && item.type === "answer" &&
      (activity?.message == null || item.entry.message == null || item.entry.message === activity.message),
  ) ? null : shown[sendIndex];
  const workingItem = activity?.state === "working" ? activeItem : null;
  const interrupted = activeItem !== null && activity?.state !== "working" ? activity?.state : undefined;
  const running = workingItem === null ? null : itemMessage(workingItem);
  const runningUnnamed = workingItem !== null && running === null;
  const openingItem = workingItem !== null && inFlightTurnKey(shown, running, runningUnnamed) === null
    ? workingItem
    : null;
  // A confirmed-empty greeting only when a real history read succeeded
  // and the last read was clean: a failed first read or a stale refresh
  // is unknown history, never "empty" — the greeting waits for an
  // actual successful refresh, and a pending send/discard cannot
  // produce it.
  const empty = loaded && history.error === undefined && items.length === 0;
  const showBootstrapCard =
    !readOnly && surface?.bootstrap === "pending" && identity.state === "ready";
  const agents = roster.data?.agents ?? [];
  const hasLiveAgents = agents.some((a) => a.running > 0);
  const hasRail =
    decisions ||
    documentFollowUps != null ||
    templateRail != null ||
    templateList != null ||
    agents.length > 0;

  return (
    <AssistantActorProvider actor={actor}>
    <StagedSourceContext.Provider value={decisions ? homeDecisions : null}>
    <main className={`home-workspace px-4 lg:px-8 pt-5 pb-3 w-full min-w-0 flex-1 min-h-0 grid gap-5 grid-rows-[minmax(0,1fr)] ${
      hasRail ? "rail:grid-cols-[minmax(0,1fr)_minmax(0,24rem)]" : ""
    }`}>
      <section className="min-w-0 flex flex-col min-h-0 gap-4" aria-label={t("assistant thread")}>
        <div className="relative flex-1 min-h-0 flex flex-col" data-chat-panel>
          <div
            className="master-heading assistant-heading pb-2.5 border-b border-ink-700/70"
            data-assistant-identity="personal"
            data-identity-state={identity.state}
          >
            <AssistantFace size={30} />
            <div className="assistant-heading-text">
              <div className="assistant-heading-name">
                <h1 className="text-section font-semibold text-ink-100" data-assistant-name>{displayName(identity, "personal", t)}</h1>
                {surface?.ok === false && (
                  <span className="chip bg-warn/10 text-warn">{t("unavailable")}</span>
                )}
                {surface?.bootstrap === "pending" && (
                  <span className="text-micro text-ink-500">{t("first-time setup")}</span>
                )}
              </div>
              <p className="assistant-scope">{scopeDescriptor("personal", t)}</p>
            </div>
            <div className="assistant-heading-actions">
              {!readOnly && surface?.bootstrap === "deferred" && identity.state === "ready" && (
                <button
                  type="button"
                  className="lnk text-label"
                  disabled={resumingBootstrap}
                  onClick={() => void resumeBootstrap()}
                >
                  {t("Finish setup")}
                </button>
              )}
              <AssistantAboutButton profileHref={personalAssistantProfileHref(actor.scope, actor.read, roster)} />
            </div>
          </div>

          {history.error?.kind === "failed" && (
            <div className="card px-3.5 py-3 mt-3 text-label text-fail break-words" role="alert">
              {t("The thread could not be read —")} {history.error.text}{" "}
              <button className="lnk" onClick={() => void store.refresh()}>{t("Retry")}</button>
            </div>
          )}
          {history.error?.kind === "stale" && (
            <div className="card px-3.5 py-3 mt-3 text-label text-warn break-words" role="alert">
              {t("The latest refresh failed — showing the last history it read.")} {history.error.text}{" "}
              <button className="lnk" onClick={() => void store.refresh()}>{t("Retry")}</button>
            </div>
          )}
          {!loaded && history.error?.kind !== "failed" && (
            <p className="text-label text-ink-500 pt-3">{t("Reading the thread…")}</p>
          )}
          {interrupted && (
            <div className="pt-2">
              <StreamBanner progress={interrupted} onRetry={() => retryActivity.current?.()} />
            </div>
          )}

          <div className="chat-scroll min-h-0 flex-1 overflow-y-auto pt-3" ref={scrollRef} data-chat-scroll>
            {showBootstrapCard && identity.state === "ready" && (
              <AssistantBootstrapCard
                identity={identity.identity}
                onComplete={completeBootstrap}
                onDefer={deferBootstrap}
              />
            )}
            {empty && !showBootstrapCard && (
              <div className="chat-empty">
                <p className="text-label text-ink-400">
                  {t("This is your private assistant — say hello to begin.")}
                </p>
              </div>
            )}
            {hidden > 0 || moreBefore ? (
              <div className="flex justify-center">
                <button className="lnk text-label disabled:opacity-50" disabled={loadingEarlier} onClick={onEarlier}>
                  {loadingEarlier ? t("Loading…") : t("Load earlier messages")}
                </button>
              </div>
            ) : null}
            <ImageConversation.Provider value={surface?.conversation ?? null}>
            <Conversation
              mode={{ kind: "home" }}
              highlightSkillCommand
              items={shown}
              running={running}
              runningUnnamed={runningUnnamed}
              activity={(item) => item === openingItem
                ? { kind: "row", position: "after", node: <ThreadTurn items={[]} running /> }
                : null}
              legacyTranscript={!items.some((item) => item.type === "answer")}
              readOnly={readOnly}
              liveAfter={liveAfter.current ?? 0}
              onOpenIssue={onOpenIssue}
              onRetry={onRetry}
              onDiscard={onDiscard}
            />
            <BrowserCard />
            </ImageConversation.Provider>
          </div>

          <div className="chatdock" ref={dockRef} data-chat-dock>
            {/* Inside the dock: it is absolutely placed, so a note before it
                would sit behind the composer. */}
            {sendNote !== null && (
              <div className="text-micro text-ink-500 px-1 pb-1" role="status">{sendNote}</div>
            )}
            <SharedComposer
              density="full"
              storeKey={`member-assist:${scope}`}
              block={readOnly ? t("The board is read-only.") : null}
              commands={[WORKSPACE_SKILLS_COMMAND]}
              onNotice={(notice) => setSendNote(displayUiNotice(notice, t))}
              onSend={({ body }) => {
                void sendToAssistant(scope, body).then((receipt) => {
                  // The actual receipt decides the note: a queued-but-
                  // unlaunched send names its cause, never a fake reply.
                  setSendNote(
                    receipt.ok && receipt.attention
                      ? `${t("Sent")} — ${receipt.attention}`
                      : null,
                  );
                  engage();
                });
              }}
              refocusAfterSend
              attach={false}
              className="card p-2.5 relative"
              placeholder={messagePlaceholder(identity, "personal", t)}
              ariaLabel={messageLabel(identity, "personal", t)}
              formProps={{ "data-composer": "" }}
            />
          </div>
        </div>
      </section>
      {hasRail && (
        <aside className="absolute rail:static min-w-0 rail:h-full rail:min-h-0">
          <NeedsRail
            documentFollowUps={documentFollowUps}
            templateRail={templateRail}
            templateCount={templateCount}
            templateList={templateList}
            hasLiveAgents={hasLiveAgents}
            onOpenIssue={onOpenIssue}
            onAskAgent={!readOnly ? (agent, update) => {
              const issues = agentOn(agent);
              const text = `Check in with ${agent.alias}${issues.length ? ` on ${issues.join(", ")}` : ""}. ${update ? `Their latest update: ${update.label}${update.text ? ` — ${update.text.slice(0, 1200)}` : ""}. ` : ""}What is the current progress, and does anything need my input?`;
              const composerKey = `member-assist:${scope}`;
              setComposerDraft(composerKey, appendComposerDraft(composerDraftState(composerKey).text, text));
              requestComposerFocus(composerKey);
            } : undefined}
            readOnly={readOnly}
            decisions={decisions}
          />
        </aside>
      )}
    </main>
    </StagedSourceContext.Provider>
    </AssistantActorProvider>
  );
}
