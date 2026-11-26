import { createContext, useContext, useEffect, useState, useSyncExternalStore, type ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import type { WorkspaceSkillSummary } from "../../lib/types";
import Link from "../../ui/Link";
import SafeLink from "../../ui/SafeLink";
import { StagedSourceContext, type StagedSource } from "../../ui/StagedChip";
import { TurnCard, type CardDensity, type CardTone } from "../../ui/TurnCard";
import { openArtifact } from "../canvas/canvasStore";
import { artifactRef } from "../canvas/feed";
import { useImageConversation } from "../canvas/imageConversation";
import { stagedChipView } from "../decisions/stagedRef";
import { failureReason, reasonText, type StagedOutcome, type StagedWrite } from "../decisions/stagedWrite";
import { openAttachmentInTab } from "./AttachmentMedia";
import type { ThreadEntry } from "./thread";
import {
  SOURCES_SHOWN,
  bodyNeedsClamp,
  outsideDomainRecipients,
  skillDraftsInTurn,
  stagedAccount,
  stagedFields,
  stagedInTurn,
  stagedStatus,
  type TurnSkillDraft,
  type TurnSource,
} from "./turnCards";

/**
 * CAD-2441 (card system phase 1, CAD-2437): under a finished answer, what
 * the turn read (Sources) and what it staged (one card per write). Both come
 * from the turn's recorded tool calls and results; see `turnCards.ts`.
 */

/**
 * CAD-2463: the file names of the thread's own attachments, by id, so an
 * attachment source reads as the file the user sent. Each surface that holds
 * the whole thread (Home, the assistant panel) provides it.
 */
export const AttachmentNamesContext = createContext<ReadonlyMap<string, string>>(new Map());

/** The record ids the app screen's `?record=` accepts (screenRoute.ts). */
const APP_RECORD_ID = /^[a-z0-9_-]{1,64}$/;

const KIND_LABEL: Record<TurnSource["kind"], string> = {
  wiki: "Wiki",
  issue: "Issue",
  attachment: "Attachment",
  artifact: "Artifact",
  web: "Web",
  app: "App",
};

function SourceChip({
  source,
  n,
  onOpenIssue,
  artifacts,
}: {
  source: TurnSource;
  n: number;
  onOpenIssue?: (id: string) => void;
  artifacts: readonly ThreadEntry[];
}) {
  const t = useTranslate();
  const conversation = useImageConversation();
  // CAD-2450: an artifact the turn holds is named by its title, not its id.
  const artifact =
    source.kind === "artifact"
      ? artifacts.map((a) => artifactRef(a.payload)).find((ref) => ref?.id === source.target) ?? null
      : null;
  const label = artifact && artifact.title !== "Untitled" ? artifact.title : source.label;
  const body: ReactNode = (
    <>
      <span className="turnsource-n num" aria-hidden>{n}</span>
      <span className="turnsource-kind">{t(KIND_LABEL[source.kind])}</span>
      <span className="turnsource-label"><bdi>{label}</bdi></span>
    </>
  );
  // The tooltip carries the full identity a shortened label leaves out.
  const tip = source.kind === "web" || source.kind === "artifact" || source.kind === "attachment" ? source.target : label;
  const attrs = { className: "turnsource", "data-source-kind": source.kind, title: tip };
  switch (source.kind) {
    case "wiki":
      return <Link {...attrs} href={`/wiki/${source.target.split("/").map(encodeURIComponent).join("/")}`}>{body}</Link>;
    case "app":
      // The app screen opens a record from `?record=` when the id is one it accepts.
      return source.app && APP_RECORD_ID.test(source.target) ? (
        <Link {...attrs} href={`/apps/${encodeURIComponent(source.app)}?${new URLSearchParams({ record: source.target })}`}>{body}</Link>
      ) : (
        <span {...attrs}>{body}</span>
      );
    case "web":
      return <SafeLink {...attrs} href={source.target}>{body}</SafeLink>;
    case "issue":
      return onOpenIssue ? (
        <button type="button" {...attrs} onClick={() => onOpenIssue(source.target)}>{body}</button>
      ) : (
        <span {...attrs}>{body}</span>
      );
    case "attachment":
      return conversation !== null ? (
        <button type="button" {...attrs} onClick={() => openAttachmentInTab(source.target, conversation)}>{body}</button>
      ) : (
        <span {...attrs}>{body}</span>
      );
    case "artifact": {
      const ref = artifact;
      return ref ? (
        <button type="button" {...attrs} onClick={() => openArtifact(ref)}>{body}</button>
      ) : (
        <span {...attrs}>{body}</span>
      );
    }
  }
}

/** "Sources · N": numbered chips that open each source by its board identity. */
export function TurnSources({
  sources,
  onOpenIssue,
  artifacts,
}: {
  sources: readonly TurnSource[];
  onOpenIssue?: (id: string) => void;
  artifacts: readonly ThreadEntry[];
}) {
  const t = useTranslate();
  const [all, setAll] = useState(false);
  if (sources.length === 0) return null;
  const shown = all ? sources : sources.slice(0, SOURCES_SHOWN);
  const hidden = sources.length - shown.length;
  return (
    <div className="turnsources" data-kind="sources" role="group" aria-label={`${t("Sources")} · ${sources.length}`}>
      <span className="turnsources-title">{t("Sources")}</span>
      {shown.map((source, i) => (
        <SourceChip key={source.key} source={source} n={i + 1} onOpenIssue={onOpenIssue} artifacts={artifacts} />
      ))}
      {hidden > 0 && (
        <button type="button" className="lnk turnsources-more" onClick={() => setAll(true)}>
          +{hidden} {t("more")}
        </button>
      )}
    </div>
  );
}

export function isExternalToolDecision(write: StagedWrite): boolean {
  return write.kind === "custom_mcp_effect" || write.kind === "connected_effect";
}

function toolReviewLabel(write: StagedWrite) {
  const preview = write.customMcp?.preview ?? write.connected?.preview;
  return {
    title: preview?.title ?? write.title,
    connection: preview?.toolkitLabel ?? write.customMcp?.connectionName ?? write.connected?.toolkit ?? write.app,
    tool: write.customMcp?.tool ?? write.connected?.slug ?? write.title,
  };
}

export function HomeToolReceipt({ write }: { write: StagedWrite }) {
  const t = useTranslate();
  const { title, connection, tool } = toolReviewLabel(write);
  const approved = write.status === "approved";
  const uncertain = write.result?.ok === false && (write.result.retry === true || write.result.code === "uncertain");
  const at = write.decidedAt === null ? null : new Date(write.decidedAt);
  return (
    <div className="min-w-0 text-right" data-chat-message="user" data-tool-decision-receipt>
      <details className="form-receipt tool-decision-receipt">
        <summary>
          <span className={approved ? "text-ok" : "text-ink-400"} aria-hidden>{approved ? "✓" : "↳"}</span>
          <strong>{t(approved ? "Approval recorded" : "Denial recorded")}</strong>
          {at !== null && Number.isFinite(at.getTime()) && (
            <time className="num text-ink-500">{at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false })}</time>
          )}
          <span className="tool-decision-subject"><bdi>{title}</bdi>{connection && <> · <bdi>{connection}</bdi></>}</span>
          {uncertain && <span className="tool-decision-subject text-warn">{t("Tool execution unconfirmed")}</span>}
        </summary>
        <dl className="turncard-fields">
          <div className="turncard-field"><dt>{t("Tool")}</dt><dd><bdi>{tool}</bdi></dd></div>
          {approved && <div className="turncard-field"><dt>{t("Approval scope")}</dt><dd>{t("This request")}</dd></div>}
          {uncertain ? (
            <div className="turncard-field"><dt>{t("Tool result")}</dt><dd>{t("The provider may have acted.")}</dd></div>
          ) : approved && write.result?.ok === false && (
            <div className="turncard-field"><dt>{t("Tool result")}</dt><dd>{reasonText(failureReason(write), t)}</dd></div>
          )}
        </dl>
      </details>
    </div>
  );
}

export function homeToolWrites(entries: readonly ThreadEntry[], rows: readonly StagedWrite[]) {
  return stagedInTurn(entries, rows).filter(isExternalToolDecision);
}

function HomeToolRequest({ write, source }: { write: StagedWrite; source: StagedSource }) {
  const t = useTranslate();
  const { title, connection } = toolReviewLabel(write);
  return (
    <TurnCard
      kind="staged"
      density="full"
      className="card turncard-staged"
      data-kind="staged-action"
      data-staged-id={write.id}
      data-outcome="pending"
      label={`${t("Tool approval")}: ${title}`}
      kindLabel={t("External tool")}
      status={{ text: t("Needs approval"), tone: "warn" }}
      footer={<button type="button" className="btn btn-sm" data-staged-review aria-haspopup="dialog" onClick={(e) => source.requestReview(write.id, e.currentTarget)}>{t("Review request")}</button>}
    >
      <p className="turncard-title"><bdi>{title}</bdi></p>
      {connection && <p className="turncard-from"><bdi>{connection}</bdi></p>}
      <p className="text-label text-ink-400">{t("This tool is paused until you decide.")}</p>
    </TurnCard>
  );
}

function HomeToolCards({ writes, source }: {
  writes: readonly StagedWrite[]; source: StagedSource;
}) {
  if (writes.length === 0) return null;
  return (
    <div className="turn-artifacts turn-staged" data-kind="staged-actions">
      {writes.map((write) => write.status === "pending"
        ? <HomeToolRequest key={write.id} write={write} source={source} />
        : <HomeToolReceipt key={write.id} write={write} />)}
    </div>
  );
}

/** Keep the regular cards for non-tools; the Home-only tool presentation
 *  may appear before an answer and never claims that approval ran the tool. */
export function HomeTurnStagedCards({ toolEntries, density, source, tools, streaming }: {
  toolEntries: readonly ThreadEntry[]; density: CardDensity; source: StagedSource;
  tools: readonly StagedWrite[]; streaming: boolean;
}) {
  return (
    <>
      {!streaming && <StagedCardsFor source={source} toolEntries={toolEntries} density={density} withoutExternalTools />}
      <HomeToolCards writes={tools} source={source} />
    </>
  );
}

const TONE: Record<StagedOutcome, CardTone> = {
  pending: "warn",
  applied: "ok",
  denied: "muted",
  failed: "fail",
  unconfirmed: "warn",
};

function StagedActionCard({ write, source, density }: { write: StagedWrite; source: StagedSource; density: CardDensity }) {
  const t = useTranslate();
  // "Not now" folds a pending preview for this view; it decides nothing.
  const [folded, setFolded] = useState(false);
  // "Show full message" lifts the body's clamp; the body is already plain text.
  const [fullBody, setFullBody] = useState(false);
  const view = stagedChipView(write);
  const receipt = view.outcome === "applied" || view.outcome === "denied";
  const collapsed = view.pending && folded;
  const { fields, more, body } = stagedFields(write);
  const preview = write.connected?.preview ?? write.customMcp?.preview;
  const title = preview?.title ?? view.title;
  const app = preview?.toolkitLabel ?? write.customMcp?.connectionName ?? write.connected?.toolkit ?? view.app;
  const account = stagedAccount(write);
  const outside = outsideDomainRecipients(write);
  const status = stagedStatus(write, view.outcome, view.status);
  const clampable = body !== null && bodyNeedsClamp(body);
  const from = account && (
    <p className="turncard-from">
      {t("from")} <bdi>{account}</bdi>
    </p>
  );
  const details = !collapsed && (
    <>
      {fields.length > 0 && (
        <dl className="turncard-fields">
          {fields.map((field, i) => (
            <div key={`${field.label}-${i}`} className="turncard-field" data-missing={field.missing || undefined} data-removed={field.removed || undefined}>
              <dt><bdi>{field.label}</bdi></dt>
              <dd>
                {field.missing ? t("Not provided") : field.removed ? (
                  <><del><bdi>{field.value}</bdi></del> <span className="text-ink-500">({t("removed")})</span></>
                ) : <bdi>{field.value}</bdi>}
              </dd>
            </div>
          ))}
        </dl>
      )}
      {body !== null && (
        <div className="turncard-message" data-message-kind={preview?.kind ?? "fields"}>
          {/* The box carries the padding and the bubble; the text inside is what clamps,
              so a clipped line never shows in the padding. */}
          <div className="turncard-message-box">
            <p className="turncard-message-text" data-clamped={clampable && !fullBody ? "" : undefined}>
              <bdi>{body}</bdi>
            </p>
          </div>
          {clampable && (
            <button type="button" className="lnk text-label" data-staged-body-toggle aria-expanded={fullBody} onClick={() => setFullBody((v) => !v)}>
              {fullBody ? t("Show less") : t("Show full message")}
            </button>
          )}
        </div>
      )}
      {outside > 0 && (
        <p className="turncard-flag" role="note" data-outside-domain={outside}>
          {outside === 1 ? t("1 recipient is outside your company domain") : `${outside} ${t("recipients are outside your company domain")}`}
        </p>
      )}
      {write.whatsapp?.destructive && write.whatsapp.warning && (
        <p className="turncard-flag" role="note" data-whatsapp-warning={write.whatsapp.operation}>
          {t(write.whatsapp.warning)}
        </p>
      )}
      {more > 0 && <p className="text-micro text-ink-500">{t("More on the review card")} (+{more})</p>}
      {!view.pending && <p className="text-micro text-ink-500">{t("Decided on the board")}</p>}
    </>
  );
  return (
    <TurnCard
      kind="staged"
      density={density}
      className="card turncard-staged"
      data-kind="staged-action"
      data-staged-id={write.id}
      data-outcome={view.outcome}
      label={`${t("Staged action")}: ${title}`}
      icon={
        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M4 14V2.5M4 3h7.5l-1.5 2.5L11.5 8H4" />
        </svg>
      }
      kindLabel={[app, t(view.kind)].filter(Boolean).join(" · ")}
      status={{ text: t(status), tone: TONE[view.outcome] }}
      data-folded={collapsed || undefined}
      footer={
        view.pending ? (
          <>
            <button type="button" className="btn btn-sm" data-staged-later aria-expanded={!collapsed} onClick={() => setFolded((v) => !v)}>
              {collapsed ? t("Show") : t("Not now")}
            </button>
            <button type="button" className="btn btn-sm btn-primary" data-staged-review onClick={(e) => source.requestReview(write.id, e.currentTarget)}>
              {t("Review")}
            </button>
          </>
        ) : undefined
      }
    >
      {receipt ? (
        <details className="turncard-receipt">
          <summary>
            <span className="turncard-title"><bdi>{title}</bdi></span>
            <span className="turncard-receipt-toggle">
              <span className="turncard-receipt-show">{t("Show details")}</span>
              <span className="turncard-receipt-hide">{t("Hide details")}</span>
              <span className="turncard-receipt-caret" aria-hidden>›</span>
            </span>
          </summary>
          <div className="turncard-receipt-details">{from}{details}</div>
        </details>
      ) : (
        <>
          <p className="turncard-title"><bdi>{title}</bdi></p>
          {from}
          {details}
        </>
      )}
    </TurnCard>
  );
}

/**
 * The writes this turn staged, each as a card under the answer. Reads the
 * decisions the surface already holds (no fetch of its own), so the status
 * follows the review card. Approving and denying stay on the board's review
 * card (CAD-1109 D3): this card only opens it.
 */
export function TurnStagedCards({ toolEntries, density }: { toolEntries: readonly ThreadEntry[]; density: CardDensity }) {
  const source = useContext(StagedSourceContext);
  if (source === null) return null;
  return <StagedCardsFor source={source} toolEntries={toolEntries} density={density} />;
}

function StagedCardsFor({ source, toolEntries, density, withoutExternalTools = false }: {
  source: StagedSource; toolEntries: readonly ThreadEntry[]; density: CardDensity; withoutExternalTools?: boolean;
}) {
  const { rows } = useSyncExternalStore(source.subscribe, source.currentDecisions, source.currentDecisions);
  const writes = stagedInTurn(toolEntries, rows).filter((write) => !withoutExternalTools || !isExternalToolDecision(write));
  if (writes.length === 0) return null;
  return (
    <div className="turn-artifacts turn-staged" data-kind="staged-actions">
      {writes.map((write) => (
        <StagedActionCard key={write.id} write={write} source={source} density={density} />
      ))}
    </div>
  );
}

export type SkillDraftLiveState = WorkspaceSkillSummary | "deleted" | undefined;

export interface SkillDraftReviewSource {
  /** Whether the current viewer may approve, edit, revoke or delete workspace skills. */
  writable: boolean;
  /** Open the shared SkillDrawer for a drafted skill. */
  openSkill: (name: string, revision?: number, opener?: HTMLElement | null) => void;
  /** Open the workspace skill library drawer (e.g. from `/skills` in the composer). */
  openLibrary?: (opener?: HTMLElement | null) => void;
  /** Subscribe to live updates from Home's skill store. */
  subscribe: (listener: () => void) => () => void;
  /** Snapshot map for useSyncExternalStore. */
  getSnapshot: () => ReadonlyMap<string, SkillDraftLiveState>;
  /** Trigger a one-time lazy list fetch on Home so older turns reflect current approval state. */
  ensureLoaded?: () => void;
}

export const SkillDraftReviewContext = createContext<SkillDraftReviewSource | null>(null);

function SkillDraftCard({
  draft,
  live,
  source,
  density,
}: {
  draft: TurnSkillDraft;
  live: SkillDraftLiveState;
  source: SkillDraftReviewSource;
  density: CardDensity;
}) {
  const t = useTranslate();
  const isDeleted = live === "deleted";
  const approvedMatch =
    !isDeleted &&
    live?.approved != null &&
    ((live.approved.revision === draft.revision && live.approved.digest === draft.digest) ||
      live.approved.revision > draft.revision);
  const latestRev = !isDeleted && live ? live.latest_revision : draft.revision;
  const state = isDeleted
    ? "deleted"
    : approvedMatch
      ? "approved"
      : !isDeleted && live && live.latest_revision > draft.revision
        ? "superseded"
        : "draft";
  const description = !isDeleted && live ? live.description : draft.description;

  const status =
    state === "deleted"
      ? { text: t("Removed"), tone: "muted" as const }
      : state === "approved" && !isDeleted && live?.approved
        ? { text: t("Approved · rev {n}").replace("{n}", String(live.approved.revision)), tone: "ok" as const }
        : { text: t("Draft · rev {n}").replace("{n}", String(latestRev)), tone: "warn" as const };

  const note =
    state === "deleted"
      ? t("This skill was removed.")
      : state === "approved" && !isDeleted && live?.approved
        ? t("Approved revision {n} is what agents read.").replace("{n}", String(live.approved.revision))
        : source.writable
          ? t("Review the instructions before approving. Only an approved revision reaches an agent.")
          : t("Saved as a draft. Ask a workspace owner to review and approve it.");

  const targetRevision =
    state === "approved" && !isDeleted && live?.approved ? live.approved.revision : latestRev;

  return (
    <TurnCard
      kind="skill-draft"
      density={density}
      className="card turncard-skill"
      data-kind="skill-draft"
      data-skill-name={draft.name}
      data-skill-revision={String(draft.revision)}
      data-skill-state={state}
      label={`${t("Workspace skill")}: ${draft.name}`}
      icon={
        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M3 2.5h10v11H3z" />
          <path d="M5.5 6h5M5.5 8.5h5M5.5 11h3" />
        </svg>
      }
      kindLabel={t("Workspace skill")}
      status={status}
      footer={
        state === "deleted" ? undefined : state === "approved" ? (
          <button
            type="button"
            className="btn btn-sm"
            data-skill-review
            onClick={(e) => source.openSkill(draft.name, targetRevision, e.currentTarget)}
          >
            {t("Inspect")}
          </button>
        ) : (
          <button
            type="button"
            className={source.writable ? "btn btn-sm btn-primary" : "btn btn-sm"}
            data-skill-review
            onClick={(e) => source.openSkill(draft.name, targetRevision, e.currentTarget)}
          >
            {source.writable ? t("Review") : t("View draft")}
          </button>
        )
      }
    >
      <p className="turncard-title"><bdi>{draft.name}</bdi></p>
      {description && <p className="text-secondary text-ink-300"><bdi>{description}</bdi></p>}
      <p className="text-micro text-ink-500">{note}</p>
    </TurnCard>
  );
}

/** Workspace skill drafts saved by `workspace_skill_draft` in this turn. */
export function TurnSkillDraftCards({ toolEntries, density }: { toolEntries: readonly ThreadEntry[]; density: CardDensity }) {
  const source = useContext(SkillDraftReviewContext);
  if (source === null) return null;
  return <SkillDraftCardsFor source={source} toolEntries={toolEntries} density={density} />;
}

function SkillDraftCardsFor({
  source,
  toolEntries,
  density,
}: {
  source: SkillDraftReviewSource;
  toolEntries: readonly ThreadEntry[];
  density: CardDensity;
}) {
  const drafts = skillDraftsInTurn(toolEntries);
  const { ensureLoaded } = source;
  useEffect(() => {
    if (drafts.length > 0) ensureLoaded?.();
  }, [drafts.length, ensureLoaded]);
  const liveMap = useSyncExternalStore(source.subscribe, source.getSnapshot, source.getSnapshot);
  if (drafts.length === 0) return null;
  return (
    <div className="turn-artifacts turn-skill-drafts" data-kind="skill-drafts">
      {drafts.map((draft) => (
        <SkillDraftCard
          key={draft.name}
          draft={draft}
          live={liveMap.get(draft.name)}
          source={source}
          density={density}
        />
      ))}
    </div>
  );
}
