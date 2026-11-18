import { useContext, useState, useSyncExternalStore, type ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import Link from "../../ui/Link";
import SafeLink from "../../ui/SafeLink";
import { StagedSourceContext, type StagedSource } from "../../ui/StagedChip";
import { TurnCard, type CardDensity, type CardTone } from "../../ui/TurnCard";
import { openArtifact } from "../canvas/canvasStore";
import { artifactRef } from "../canvas/feed";
import { useImageConversation } from "../canvas/imageConversation";
import { stagedChipView } from "../decisions/stagedRef";
import type { StagedOutcome, StagedWrite } from "../decisions/stagedWrite";
import { openAttachmentInTab } from "./AttachmentMedia";
import type { ThreadEntry } from "./thread";
import { SOURCES_SHOWN, stagedFields, stagedInTurn, type TurnSource } from "./turnCards";

/**
 * CAD-2441 (card system phase 1, CAD-2437): under a finished answer, what
 * the turn read (Sources) and what it staged (one card per write). Both come
 * from the turn's recorded tool calls and results; see `turnCards.ts`.
 */

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
  const body: ReactNode = (
    <>
      <span className="turnsource-n num" aria-hidden>{n}</span>
      <span className="turnsource-kind">{t(KIND_LABEL[source.kind])}</span>
      <span className="turnsource-label"><bdi>{source.label}</bdi></span>
    </>
  );
  const attrs = { className: "turnsource", "data-source-kind": source.kind, title: source.label };
  switch (source.kind) {
    case "wiki":
      return <Link {...attrs} href={`/wiki/${source.target.split("/").map(encodeURIComponent).join("/")}`}>{body}</Link>;
    case "app":
      return <Link {...attrs} href={`/apps/${encodeURIComponent(source.target)}`}>{body}</Link>;
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
      const entry = artifacts.find((a) => artifactRef(a.payload)?.id === source.target);
      const ref = entry ? artifactRef(entry.payload) : null;
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

const TONE: Record<StagedOutcome, CardTone> = {
  pending: "warn",
  applied: "ok",
  denied: "muted",
  failed: "fail",
  unconfirmed: "warn",
};

function StagedActionCard({ write, source, density }: { write: StagedWrite; source: StagedSource; density: CardDensity }) {
  const t = useTranslate();
  // "Not now" folds the card to its header for this view; it decides nothing.
  const [folded, setFolded] = useState(false);
  const view = stagedChipView(write);
  const { fields, more } = stagedFields(write);
  const preview = write.connected?.preview;
  const title = preview?.title ?? view.title;
  const app = preview?.toolkitLabel ?? view.app;
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
      status={{ text: t(view.status), tone: TONE[view.outcome] }}
      data-folded={folded || undefined}
      footer={
        view.pending ? (
          <>
            <button type="button" className="btn btn-sm" data-staged-later aria-expanded={!folded} onClick={() => setFolded((v) => !v)}>
              {folded ? t("Show") : t("Not now")}
            </button>
            <button type="button" className="btn btn-sm btn-primary" data-staged-review onClick={(e) => source.requestReview(write.id, e.currentTarget)}>
              {t("Review")}
            </button>
          </>
        ) : undefined
      }
    >
      <p className="turncard-title"><bdi>{title}</bdi></p>
      {!folded && (
        <>
      {fields.length > 0 && (
        <dl className="turncard-fields">
          {fields.map((field, i) => (
            <div key={`${field.label}-${i}`} className="turncard-field" data-missing={field.missing || undefined}>
              <dt><bdi>{field.label}</bdi></dt>
              <dd>{field.missing ? t("Not provided") : <bdi>{field.value}</bdi>}</dd>
            </div>
          ))}
        </dl>
      )}
      {more > 0 && <p className="text-micro text-ink-500">{t("More on the review card")} (+{more})</p>}
        </>
      )}
      {!view.pending && <p className="text-micro text-ink-500">{t("Decided on the board")}</p>}
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

function StagedCardsFor({ source, toolEntries, density }: { source: StagedSource; toolEntries: readonly ThreadEntry[]; density: CardDensity }) {
  const { rows } = useSyncExternalStore(source.subscribe, source.currentDecisions, source.currentDecisions);
  const writes = stagedInTurn(toolEntries, rows);
  if (writes.length === 0) return null;
  return (
    <div className="turn-artifacts turn-staged" data-kind="staged-actions">
      {writes.map((write) => (
        <StagedActionCard key={write.id} write={write} source={source} density={density} />
      ))}
    </div>
  );
}
