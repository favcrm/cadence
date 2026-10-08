import type { AudienceBaseInput } from "../audienceClient";
import type { CampaignTab } from "./readiness";

/**
 * One-shot hand-off from the New campaign dialog (CAD-1058) to the
 * saved campaign's detail: which tab to open and any validated starter
 * audience criteria. It lives in memory, never in the URL, so a stale
 * value can never steer an unrelated campaign: it is keyed to the new
 * campaign's id and cleared by the detail after it mounts.
 */
export interface CampaignLanding {
  campaignId: string;
  tab: CampaignTab;
  segmentId: string | null;
  audienceBase?: AudienceBaseInput | null;
  exclusionListId?: string | null;
  senderBindingId?: string | null;
}

let pending: CampaignLanding | null = null;

export function setLanding(landing: CampaignLanding): void {
  pending = landing;
}

/** Read without consuming (safe under StrictMode double invoke). */
export function peekLanding(campaignId: string): CampaignLanding | null {
  return pending !== null && pending.campaignId === campaignId ? pending : null;
}

export function clearLanding(campaignId: string): void {
  if (pending !== null && pending.campaignId === campaignId) pending = null;
}
