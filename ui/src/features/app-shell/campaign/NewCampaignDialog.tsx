import { useEffect, useRef, useState } from "react";
import { api, ApiError } from "../../../lib/api";
import { resources } from "../../../lib/resources";
import Button from "../../../ui/Button";
import { Select } from "../../../ui/Select";
import { MASTER } from "../../home/master";
import { newMessageId } from "../../home/thread";
import { audienceClient, type AudienceScope } from "../audienceClient";
import { checkCampaignName, friendlyCampaignError, CAMPAIGN_NAME_MAX } from "../campaignGrammar";
import { contentClient } from "../contentClient";
import { openCampaignConversation } from "../conversationClient";
import { friendlyAudienceError, newAudienceId } from "../segmentGrammar";
import Field from "../shared/Field";
import { setLanding } from "./landing";

/**
 * New campaign (CAD-1058): a short modal — a human name, how to start,
 * an optional audience. It replaces the old long New page. Nothing is
 * sent from here: the campaign is saved as an unapproved draft, the
 * operator lands on its Email tab, and every send gate stays where it
 * was.
 *
 * Start with:
 * - Assistant draft: the campaign is created with a placeholder, then
 *   the brief goes to the scoped CRM chat so the assistant drafts a
 *   pending proposal (inert until the operator applies it);
 * - Paste HTML: the operator html save (host-sanitised);
 * - Blank: one heading and one paragraph block.
 * The generated campaign id is shown only under Details on the
 * campaign page.
 */

export type StartWith = "ai" | "html" | "blank";

const STARTS: { key: StartWith; title: string; hint: string; action: string }[] = [
  { key: "ai", title: "✦ Assistant draft", hint: "Describe it; the assistant writes it", action: "Create and draft" },
  { key: "html", title: "Paste HTML", hint: "Bring an existing email", action: "Create from HTML" },
  { key: "blank", title: "Blank", hint: "Write it with blocks", action: "Create blank campaign" },
];

/** The primary button text follows the choice. */
export function startAction(start: StartWith): string {
  return STARTS.find((s) => s.key === start)!.action;
}

export default function NewCampaignDialog({
  scope,
  initialSegmentId,
  onCreated,
  onCancel,
}: {
  scope: AudienceScope;
  /** `?segment=` from the segment drawer's "Use in campaign". */
  initialSegmentId: string | null;
  onCreated: (campaignId: string) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState("");
  const [start, setStart] = useState<StartWith>("ai");
  const [brief, setBrief] = useState("");
  const [html, setHtml] = useState("");
  const [segmentId, setSegmentId] = useState(initialSegmentId ?? "");
  const [segments, setSegments] = useState<{ id: string; name: string }[]>([]);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [nameError, setNameError] = useState<string | null>(null);
  // The id is generated once; a retry after a partial failure reuses it
  // and never saves the campaign twice.
  const [campaignId] = useState(() => newAudienceId("cmp"));
  const [saved, setSaved] = useState(false);
  const nameRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    nameRef.current?.focus();
  }, []);
  useEffect(() => {
    const controller = new AbortController();
    audienceClient
      .segmentList(scope)
      .then((value) => {
        if (controller.signal.aborted) return;
        const rows = (value as { segments?: unknown } | null)?.segments;
        setSegments(
          Array.isArray(rows)
            ? rows.flatMap((row: { id?: unknown; name?: unknown }) =>
                typeof row.id === "string" && typeof row.name === "string"
                  ? [{ id: row.id, name: row.name }]
                  : [],
              )
            : [],
        );
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(friendlyAudienceError(e));
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope.installId, scope.contextId]);

  const land = () => {
    setLanding({ campaignId, tab: "email", segmentId: segmentId === "" ? null : segmentId });
    onCreated(campaignId);
  };

  const submit = async () => {
    setError(null);
    const trimmed = name.trim();
    try {
      checkCampaignName(trimmed);
    } catch (e) {
      setNameError(e instanceof ApiError ? e.message : "Give the campaign a name.");
      nameRef.current?.focus();
      return;
    }
    setNameError(null);
    if (start === "ai" && brief.trim() === "") {
      setError("Describe what the email should say so the assistant can draft it.");
      return;
    }
    if (start === "html" && html.trim() === "") {
      setError("Paste the email HTML to start from it.");
      return;
    }
    setPending(true);
    try {
      if (!saved) {
        const body =
          start === "html"
            ? { html }
            : {
                blocks: [
                  { type: "heading" as const, text: trimmed },
                  { type: "paragraph" as const, text: "Write your message here." },
                ],
              };
        await contentClient.save(scope, {
          campaignId,
          name: trimmed,
          subject: trimmed,
          ...body,
        });
        setSaved(true);
      }
      if (start === "ai") {
        const text = `Draft the email for campaign ${campaignId} (“${trimmed}”): ${brief.trim()}`;
        try {
          // CAD-1098: the brief goes to this campaign's own conversation
          // (opened idempotently, then selected for the chat panel).
          const conversation = await openCampaignConversation(scope.installId, scope.contextId, campaignId);
          await api.threadSend(
            MASTER,
            text,
            newMessageId(),
            undefined,
            { install_id: scope.installId, context_id: scope.contextId },
            conversation ?? undefined,
          );
        } catch (e) {
          setError(
            `The campaign is saved, but the brief was not sent to the assistant: ${
              e instanceof ApiError ? e.message : "request failed"
            }`,
          );
          return;
        }
        void resources.masterState.refresh();
      }
      land();
    } catch (e) {
      setError(friendlyCampaignError(e));
    } finally {
      setPending(false);
    }
  };

  const action = saved && start === "ai" ? "Send brief again" : startAction(start);
  return (
    <div className="crm-confirm-wrap" role="presentation" data-new-campaign>
      <div className="crm-confirm-scrim" onClick={pending ? undefined : onCancel} />
      <form
        className="card crm-newc grid gap-3"
        noValidate
        role="dialog"
        aria-modal="true"
        aria-labelledby="crm-newc-title"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
        onKeyDown={(e) => {
          if (e.key === "Escape" && !pending) {
            e.stopPropagation();
            onCancel();
          }
        }}
      >
        <div className="crm-newc-head">
          <div>
            <h4 id="crm-newc-title" className="text-cardtitle font-medium text-ink-100">
              New campaign
            </h4>
            <p className="text-label text-ink-400">
              Name it and choose how to start. You can change everything later.
            </p>
          </div>
          <Button size="sm" variant="ghost" aria-label="Close" onClick={onCancel} disabled={pending}>
            ✕
          </Button>
        </div>
        <Field label="Name" required error={nameError ?? undefined} className="crm-field">
          {(c) => (
            <input
              {...c}
              ref={nameRef}
              className="field"
              value={name}
              onChange={(e) => setName(e.target.value)}
              maxLength={CAMPAIGN_NAME_MAX}
              autoComplete="off"
              disabled={pending || saved}
            />
          )}
        </Field>
        <div role="radiogroup" aria-label="Start with" className="grid gap-1">
          <span className="text-label text-ink-300">Start with</span>
          <div className="crm-starts">
            {STARTS.map((s) => (
              <button
                key={s.key}
                type="button"
                role="radio"
                aria-checked={start === s.key}
                className="crm-start"
                data-start={s.key}
                disabled={pending || saved}
                onClick={() => setStart(s.key)}
              >
                <span className="crm-start-title">{s.title}</span>
                <span className="crm-start-hint">{s.hint}</span>
              </button>
            ))}
          </div>
        </div>
        {start === "ai" && (
          <Field label="What should it say?" className="crm-field">
            {(c) => (
              <textarea
                {...c}
                className="field"
                rows={3}
                value={brief}
                onChange={(e) => setBrief(e.target.value)}
                maxLength={2000}
                disabled={pending && !saved}
                placeholder="Welcome new customers warmly, keep it short, and link to the booking page."
              />
            )}
          </Field>
        )}
        {start === "html" && (
          <Field
            label="HTML"
            hint="Scripts, forms and tracking pixels are removed; the unsubscribe footer is added for you"
            className="crm-field"
          >
            {(c) => (
              <textarea
                {...c}
                className="field num"
                rows={6}
                value={html}
                onChange={(e) => setHtml(e.target.value)}
                disabled={pending || saved}
                placeholder="<h1>Hello</h1>…"
              />
            )}
          </Field>
        )}
        {start === "blank" && (
          <p className="text-secondary text-ink-400">Opens the editor with a heading and a paragraph.</p>
        )}
        <Field label="Audience" hint="optional — you can choose later" className="crm-field">
          {(c) => (
            <Select
              id={c.id}
              value={segmentId}
              onChange={setSegmentId}
              options={[
                { value: "", label: "Decide later" },
                ...segments.map((row) => ({ value: row.id, label: row.name })),
                // A preselected segment that is not in the list yet stays pickable.
                ...(segmentId !== "" && !segments.some((row) => row.id === segmentId)
                  ? [{ value: segmentId, label: segmentId }]
                  : []),
              ]}
              aria-label="Audience"
              disabled={pending || saved}
              full
            />
          )}
        </Field>
        {error && (
          <p className="text-label text-fail" role="alert">
            {error}
          </p>
        )}
        <div className="crm-newc-foot">
          <span className="text-label text-ink-400">Nothing is sent from here.</span>
          {saved && start === "ai" && (
            <Button size="sm" variant="ghost" onClick={land} disabled={pending}>
              Open without the brief
            </Button>
          )}
          <Button size="sm" variant="ghost" onClick={onCancel} disabled={pending}>
            Cancel
          </Button>
          <Button size="sm" variant="primary" type="submit" loading={pending} disabled={pending}>
            {action}
          </Button>
        </div>
      </form>
    </div>
  );
}
