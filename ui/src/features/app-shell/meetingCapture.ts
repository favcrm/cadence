export const SPEECH_BATCH_MS = 8000;

export interface SpeechDraft {
  lines: string[];
  pending: string;
  flushed: number;
}

export function emptySpeechDraft(): SpeechDraft {
  return { lines: [], pending: "", flushed: 0 };
}

export function speechStamp(elapsedMs: number): string {
  const total = Math.max(0, Math.floor(elapsedMs / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}

/** Append one final phrase. Interim guesses are ignored by the caller. */
export function hearSpeech(draft: SpeechDraft, words: string): SpeechDraft {
  const next = words.trim().replace(/\s+/g, " ");
  if (!next) return draft;
  return { ...draft, pending: draft.pending ? `${draft.pending} ${next}` : next };
}

/**
 * Write one line per completed 8-second block. A block with no words
 * advances the clock without a line. `finish` writes any words heard
 * after the last block, using the elapsed stamp.
 */
export function flushSpeech(draft: SpeechDraft, elapsedMs: number, finish: boolean): SpeechDraft {
  const due = Math.floor(Math.max(0, elapsedMs) / SPEECH_BATCH_MS);
  let next = draft;
  while (next.flushed < due) {
    const stamp = speechStamp(next.flushed * SPEECH_BATCH_MS);
    next = next.pending
      ? { lines: [...next.lines, `${stamp} ${next.pending}`], pending: "", flushed: next.flushed + 1 }
      : { ...next, flushed: next.flushed + 1 };
  }
  if (finish && next.pending) {
    next = { lines: [...next.lines, `${speechStamp(elapsedMs)} ${next.pending}`], pending: "", flushed: next.flushed };
  }
  return next;
}

/** Not recording offers Record. A recording with words writes the minutes. Silence only stops. */
export function recordAction(recording: boolean, hasTranscript: boolean): "record" | "write" | "stop" {
  if (!recording) return "record";
  return hasTranscript ? "write" : "stop";
}

export function speechTranscript(base: string, draft: SpeechDraft): string {
  const head = base.trim();
  const body = draft.lines.join("\n");
  if (head && body) return `${head}\n${body}`;
  return head || body;
}

type SpeechResult = { isFinal: boolean; 0: { transcript: string } };
type SpeechEvent = { resultIndex: number; results: ArrayLike<SpeechResult> };

export type SpeechSession = {
  continuous: boolean;
  interimResults: boolean;
  lang: string;
  onresult: ((event: SpeechEvent) => void) | null;
  onerror: (() => void) | null;
  onend: (() => void) | null;
  start: () => void;
  stop: () => void;
};

type SpeechCtor = new () => SpeechSession;

export function speechRecognitionCtor(target: Window = window): SpeechCtor | null {
  const host = target as Window & { SpeechRecognition?: SpeechCtor; webkitSpeechRecognition?: SpeechCtor };
  return host.SpeechRecognition ?? host.webkitSpeechRecognition ?? null;
}
