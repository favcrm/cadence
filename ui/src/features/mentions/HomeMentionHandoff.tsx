import { useCallback, useMemo, useRef, useState, type RefObject } from "react";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { startsWithNote, type MentionCandidate } from "./mentions";

/**
 * Find non-silent `@teammate` mentions inside a private Home draft,
 * ignoring email addresses (`foo@bar`) and `/note` silent drafts.
 */
export function findMentionedTeammates(
  draft: string,
  candidates: readonly MentionCandidate[],
): MentionCandidate[] {
  if (!draft.trim() || startsWithNote(draft)) return [];
  const humans = candidates.filter((c) => c.kind === "human" && c.active);
  if (humans.length === 0) return [];
  const matches: MentionCandidate[] = [];
  const seen = new Set<string>();
  const re = /(?:^|[^A-Za-z0-9_.])@([A-Za-z0-9_-]+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(draft)) !== null) {
    const token = m[1]!.toLowerCase();
    const hit = humans.find((c) => c.handle.toLowerCase() === token);
    if (hit && !seen.has(hit.id)) {
      seen.add(hit.id);
      matches.push(hit);
    }
  }
  return matches;
}

/** Consent-gated CAD-2476 preview card when mentioning a colleague inside private Home. */
export function HomeMentionHandoffCard({
  teammates,
  sendBlockedForConsent,
  onOpenChannels,
  onKeepPrivate,
}: {
  teammates: readonly MentionCandidate[];
  sendBlockedForConsent: boolean;
  onOpenChannels: () => void;
  onKeepPrivate: () => void;
}) {
  const t = useTranslate();
  if (teammates.length === 0) return null;
  const names = teammates.map((m) => `@${m.handle}`).join(", ");
  return (
    <div
      className="home-handoff-preview card p-2.5 mt-2"
      role={sendBlockedForConsent ? "alert" : "status"}
      data-home-handoff-preview=""
    >
      <div className="home-handoff-head">
        <strong>{t("Colleague mention in private Home")}</strong>
        <span className="home-handoff-handles">{names}</span>
      </div>
      <p className="text-micro text-ink-400 m-0">
        {sendBlockedForConsent
          ? t("Choose whether to continue in Channels or keep this message strictly private with your assistant (private Home history is never shared).")
          : t("Mentioning a colleague here stays private unless you hand off to Channels. Private Home history is never exposed.")}
      </p>
      <div className="home-handoff-actions">
        <Button variant="primary" size="sm" onClick={onOpenChannels}>
          {t("Open in Channels")}
        </Button>
        <Button variant="secondary" size="sm" onClick={onKeepPrivate}>
          {t("Keep private in Home")}
        </Button>
      </div>
    </div>
  );
}

/** Shared private-Home colleague mention detection + consent hold state. */
export function useHomeMentionHandoff(
  candidates: readonly MentionCandidate[],
  dockRef: RefObject<HTMLDivElement | null>,
) {
  const [draftText, setDraftText] = useState("");
  const [handoffHold, setHandoffHold] = useState(false);
  const approvedPrivateDraftRef = useRef<string | null>(null);
  const mentionedTeammates = useMemo(
    () => findMentionedTeammates(draftText, candidates),
    [draftText, candidates],
  );
  const onDraftChange = useCallback(
    (next: string) => {
      setDraftText(next);
      if (next.trim() !== approvedPrivateDraftRef.current) {
        approvedPrivateDraftRef.current = null;
      }
      if (findMentionedTeammates(next, candidates).length === 0) {
        setHandoffHold(false);
      }
    },
    [candidates],
  );
  const beforeSend = useCallback(
    (outgoing: string) => {
      const normalized = outgoing.trim();
      if (
        findMentionedTeammates(normalized, candidates).length > 0 &&
        approvedPrivateDraftRef.current !== normalized
      ) {
        setHandoffHold(true);
        return false;
      }
      approvedPrivateDraftRef.current = null;
      setHandoffHold(false);
      return true;
    },
    [candidates],
  );
  const onKeepPrivate = useCallback(() => {
    approvedPrivateDraftRef.current = draftText.trim();
    setHandoffHold(false);
    dockRef.current?.querySelector<HTMLFormElement>("form[data-composer]")?.requestSubmit();
  }, [draftText, dockRef]);

  return {
    mentionedTeammates,
    handoffHold,
    onDraftChange,
    beforeSend,
    onKeepPrivate,
  };
}
