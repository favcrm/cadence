import type { ContentDoc } from "../campaignGrammar";
import type { SmtpBinding, TestSendReceipt } from "../sendClient";
import { EMAIL_SENDING_HREF } from "../../settings/emailSendingView";

/**
 * The send prerequisites as data (CAD-1055). One list feeds both the
 * Overview "Ready to send" checklist and the Final send panel's gate,
 * so the two can never disagree. `reason` is the long-standing
 * "Prepare stays unavailable — missing" sentence; the daemon still
 * re-verifies every input at prepare and approve.
 */

export type CampaignTab = "overview" | "email" | "audience" | "activity";

/** Where an unmet item is fixed: another tab, or a panel on Overview. */
export type FixTarget =
  | { kind: "tab"; tab: CampaignTab; label: string }
  | { kind: "anchor"; id: string; label: string }
  | { kind: "href"; href: string; label: string };

export interface ReadinessItem {
  key: "draft" | "approval" | "freeze" | "sender" | "test";
  /** Plain-language checklist line. */
  label: string;
  done: boolean;
  /** Why it is unmet; null for the draft row (not a prepare gate). */
  reason: string | null;
  fix: FixTarget | null;
}

export interface ReadinessInput {
  doc: ContentDoc | null;
  freezeId: string;
  freeze: { valid: boolean | null } | null;
  /** `undefined` = still loading, `null` = none bound. */
  binding: SmtpBinding | null | undefined;
  testEvidence: TestSendReceipt | null;
}

export const TEST_ANCHOR = "cmp-test-section";
export const APPROVAL_ANCHOR = "cmp-approval-section";

export function sendReadiness(input: ReadinessInput): ReadinessItem[] {
  const { doc, freezeId, freeze, binding, testEvidence } = input;
  const frozen = freezeId.trim();

  const approved = doc !== null && doc.approval.valid && doc.approval.revision === doc.revision;

  let freezeReason: string | null = null;
  if (frozen === "") freezeReason = "a named audience freeze";
  else if (freeze === null) freezeReason = `freeze ${frozen} rechecked below (its validity is unverified)`;
  else if (freeze.valid !== true) freezeReason = `freeze ${frozen} reporting valid`;

  let senderReason: string | null = null;
  if (binding === undefined) senderReason = "the sender binding read (still loading)";
  else if (binding === null || binding.state !== "live" || binding.usable === false)
    senderReason = binding !== null && binding.usable === false ? "the platform sender (SMTP can't send from a hosted workspace)" : "a live SMTP sender binding";

  let testReason: string | null = null;
  if (testEvidence === null) testReason = "an accepted test send of this content and binding";
  else if (
    doc !== null &&
    binding !== null &&
    binding !== undefined &&
    (testEvidence.contentDigest !== doc.contentDigest || testEvidence.linkDigest !== binding.digest)
  ) {
    testReason = "a test send accepted against this exact content revision and binding";
  }

  return [
    {
      key: "draft",
      label: "Email drafted",
      done: doc !== null,
      reason: null,
      fix: { kind: "tab", tab: "email", label: "Review" },
    },
    {
      key: "approval",
      label: "Content approved",
      done: approved,
      reason: approved ? null : "content approved at the current revision",
      fix: doc === null ? null : { kind: "anchor", id: APPROVAL_ANCHOR, label: "Approve" },
    },
    {
      key: "freeze",
      label: "Audience frozen and still valid",
      done: freezeReason === null,
      reason: freezeReason,
      fix: { kind: "tab", tab: "audience", label: "Fix" },
    },
    {
      key: "sender",
      label: "Email sender connected",
      done: senderReason === null,
      reason: senderReason,
      fix: { kind: "href", href: EMAIL_SENDING_HREF, label: "Set up sending" },
    },
    {
      key: "test",
      label: "Test email accepted",
      done: testReason === null,
      reason: testReason,
      fix: { kind: "anchor", id: TEST_ANCHOR, label: "Send a test" },
    },
  ];
}

/** The unmet prepare gates, in checklist order. */
export function missingReasons(items: ReadinessItem[]): string[] {
  return items.flatMap((item) => (!item.done && item.reason !== null ? [item.reason] : []));
}
