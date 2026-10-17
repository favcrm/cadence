const MINUTES_HEADINGS = ["Summary", "Decisions", "Follow-ups", "Transcript"];

/** The consolidate workflow's page, or null when the text is not that shape. */
export function minutesSections(text: string): { heading: string; body: string }[] | null {
  const parts = text.split(/^## /m).slice(1);
  if (parts.length !== MINUTES_HEADINGS.length) return null;
  const sections = parts.map((part) => {
    const breakAt = part.indexOf("\n");
    const heading = (breakAt === -1 ? part : part.slice(0, breakAt)).trim();
    const body = (breakAt === -1 ? "" : part.slice(breakAt + 1)).trim();
    return { heading, body };
  });
  if (sections.some((section, index) => section.heading !== MINUTES_HEADINGS[index])) return null;
  return sections;
}

/** A typed subject is kept. A blank subject takes the page heading only after the minutes name it. */
export function meetingTitle(subject: string, page: string | null): { title: string; origin: "Your title" | "From the discussion" | null } {
  const typed = subject.trim();
  if (typed && typed !== "Untitled meeting") return { title: typed, origin: "Your title" };
  const heading = page?.match(/^#\s+(.+)$/m)?.[1]?.trim() ?? "";
  if (heading && heading !== "<title>" && heading !== "Untitled meeting") return { title: heading, origin: "From the discussion" };
  return { title: "Untitled meeting", origin: null };
}

/** The open meeting always reads its page. A list row reads one only when the title still depends on it. */
export function needsMinutesPage(subject: string, selected: boolean, hasArtifact: boolean): boolean {
  if (!hasArtifact) return false;
  if (selected) return true;
  return meetingTitle(subject, null).origin === null;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** UTC day from a run's created epoch seconds, or null when the stamp is missing. */
export function meetingWhen(created: number | undefined): string | null {
  if (created === undefined || !Number.isFinite(created)) return null;
  const date = new Date(created * 1000);
  if (Number.isNaN(date.getTime())) return null;
  return `${date.getUTCDate()} ${MONTHS[date.getUTCMonth()]}`;
}

const STAMP_LINE = /^(?:[-*]\s+)?(\d{1,3}):(\d{2})\b/;

/** Latest transcript stamp. A 00:00 opening, or text with no stamp, is not a length. */
export function meetingLength(transcript: string | null | undefined): string | null {
  if (!transcript) return null;
  let last = 0;
  let stamped = false;
  for (const line of transcript.split("\n")) {
    const match = STAMP_LINE.exec(line.trim());
    if (!match) continue;
    const seconds = Number(match[2]);
    if (seconds > 59) continue;
    stamped = true;
    const total = Number(match[1]) * 60 + seconds;
    if (total > last) last = total;
  }
  if (!stamped || last === 0) return null;
  const minutes = Math.floor(last / 60);
  const seconds = last % 60;
  if (minutes === 0) return seconds === 1 ? "1 sec" : `${seconds} sec`;
  if (seconds === 0) return minutes === 1 ? "1 min" : `${minutes} min`;
  return `${minutes} min ${seconds} sec`;
}

/** Lines that were sent for consolidation. Blank lines are dropped. This is not a minutes page. */
export function sentTranscriptLines(transcript: string | null | undefined): string[] {
  if (!transcript) return [];
  return transcript.split("\n").map((line) => line.trim()).filter((line) => line.length > 0);
}

/** The failure sentence recorded on the run, or null when there is none. */
export function failureReason(reason: string | undefined): string | null {
  const text = reason?.trim() ?? "";
  return text.length > 0 ? text : null;
}

/** Operator-facing run state. In-flight states stay one label. */
export function meetingStateLabel(state: string): string {
  if (state === "succeeded") return "Ready";
  if (state === "failed") return "Failed";
  if (state === "cancelled") return "Cancelled";
  return "Consolidating";
}

const SETTLED_RUN: Record<string, true> = {
  succeeded: true,
  failed: true,
  cancelled: true,
};

/** True while any run can still gain a minutes page. */
export function meetingListOpen(states: readonly string[]): boolean {
  return states.some((state) => !SETTLED_RUN[state]);
}

const LIST_SECTION: Record<string, true> = {
  Decisions: true,
  "Follow-ups": true,
  Transcript: true,
};

/** One item per line. Summary stays prose, so this returns null. */
export function minutesLines(heading: string, body: string): string[] | null {
  if (!LIST_SECTION[heading]) return null;
  return body
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .map((line) => (heading === "Transcript" ? line : line.replace(/^[-*]\s+/, "")));
}

/** The two review decisions the minutes workflow returns. Anything else is shown as written. */
export function reviewLabel(decision: string | undefined): string | null {
  const value = decision?.trim() ?? "";
  if (!value) return null;
  if (value === "approve") return "Approved";
  if (value === "revise") return "Needs revision";
  return value;
}

/** A failed run, or one sent back for revision, can be copied onto Record. An approved page cannot. */
export function rewriteOffer(state: string, decision: string | undefined, transcript: string | null | undefined): boolean {
  if (sentTranscriptLines(transcript).length === 0) return false;
  if (reviewLabel(decision) === "Needs revision") return true;
  return state === "failed";
}

/** Why an open meeting has no minutes page. A real minutes page returns null. */
export function minutesGap(state: string, hasArtifact: boolean, page: string | null | undefined): "loading" | "unready" | "unwritten" | "unloadable" | "absent" | "unshaped" | null {
  if (typeof page === "string" && page.trim().length > 0) return minutesSections(page) ? null : "unshaped";
  if (hasArtifact && page === undefined) return "loading";
  if (hasArtifact) return "unloadable";
  if (state === "failed" || state === "cancelled") return "unwritten";
  if (state === "succeeded") return "absent";
  return "unready";
}

/** List and meeting header. A finished run with no page is not called ready. */
export function meetingStatus(state: string, hasArtifact: boolean, page: string | null | undefined): string {
  const gap = minutesGap(state, hasArtifact, page);
  if (gap === "absent") return "No minutes page";
  if (gap === "unloadable") return "Minutes unavailable";
  if (gap === "unshaped" && state === "succeeded") return "Not a minutes page";
  return meetingStateLabel(state);
}

/** The sentence under an open meeting that has no page yet. */
export function minutesGapNote(gap: "unready" | "unwritten" | "unloadable" | "absent" | "unshaped", sent: boolean): string {
  if (gap === "unshaped") return "This page is not minutes.";
  const lines = sent ? " These are the lines that were sent." : "";
  if (gap === "unloadable") return `The minutes page could not be loaded.${lines}`;
  if (gap === "unwritten") return `Minutes were not written.${lines}`;
  if (gap === "absent") return `This run has no minutes page.${lines}`;
  return sent ? "Minutes are not ready yet. These are the lines that were sent." : "Minutes are not ready yet.";
}

/** Stored failure and review text for a draft copied onto Record. Blank text is dropped, and the same sentence is not repeated. */
export function rewriteNote(rationale: string | undefined, failure: string | undefined): string | null {
  const review = rationale?.trim() ?? "";
  const why = failure?.trim() ?? "";
  if (review && why && review !== why) return `${review}\n${why}`;
  const note = review || why;
  return note || null;
}

/** A typed subject is copied. Untitled stays blank so the next page can name it. */
export function rewriteDraft(subject: string, people: string | undefined, transcript: string | undefined): { title: string; people: string; transcript: string } {
  const typed = subject.trim();
  return {
    title: typed && typed !== "Untitled meeting" ? typed : "",
    people: people?.trim() ?? "",
    transcript: transcript ?? "",
  };
}
