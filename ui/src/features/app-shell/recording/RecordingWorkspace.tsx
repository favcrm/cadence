import { useCallback, useEffect, useMemo, useState } from "react";
import Button from "../../../ui/Button";
import { recordingClient, type RecordingBase, type RecordingConsent, type RecordingDetail, type RecordingQuote, type RecordingSummary, type Segment } from "./recordingClient";
import { useRecordingSession, type SessionPhase } from "./useRecordingSession";
import "./recording.css";

interface Props { installId: string; contextId: string; title: string; declaration: { label?: string; title?: string } }
type View = { kind: "list" } | { kind: "new"; visit: number } | { kind: "detail"; id: string };
const LABEL: Record<SessionPhase, string> = { ready: "Ready", starting: "Starting", recording: "Recording", paused: "Paused", saving: "Saving recording", finishing: "Finishing", transcribing: "Transcribing", consolidating: "Writing minutes", failed: "Needs attention", error: "Needs attention" };
const clock = (ms: number) => { const seconds = Math.floor(ms / 1000); return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`; };
export function formatUsdMinor(minor: number): string {
  return `$${Math.floor(minor / 1_000_000)}.${String(minor % 1_000_000).padStart(6, "0").replace(/0+$/, "").padEnd(2, "0")}`;
}
function ErrorMessage({ text }: { text: string | null }) { return text ? <p className="recording-error" role="alert">{text}</p> : null; }
function Transcript({ segments, empty = "Lines appear here as each audio batch is saved." }: { segments: Segment[]; empty?: string }) {
  const confirmed = segments.filter((segment) => segment.state === "confirmed" && segment.text?.trim());
  return confirmed.length ? <ol className="decisions" role="log" aria-live="polite" aria-relevant="additions">
    {confirmed.map((segment) => <li id={`recording-segment-${segment.sequence}`} key={segment.sequence}><span className="cite">[{segment.sequence + 1}] {clock(segment.start_ms)}</span><span>{segment.text}</span></li>)}
  </ol> : <p className="muted">{empty}</p>;
}

/** Generic trusted host view selected only by a verified recording declaration.
 * The shared assistant stays mounted; no app-name switch or iframe microphone. */
export default function RecordingWorkspace({ installId, contextId, title, declaration }: Props) {
  const base = useMemo(() => ({ installId, contextId }), [installId, contextId]);
  const [view, setView] = useState<View>({ kind: "list" });
  const [rows, setRows] = useState<RecordingSummary[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (view.kind !== "list") return;
    let current = true;
    setLoading(true);
    void recordingClient.list(base, null).then((response) => {
      if (!current) return;
      if (!response.ok) { setError(response.error.text); return; }
      setRows(response.value.recordings); setCursor(response.value.next_cursor); setError(null);
    }).catch(() => { if (current) setError("Meetings could not be loaded. Retry the connection."); })
      .finally(() => { if (current) setLoading(false); });
    return () => { current = false; };
  }, [base, view]);
  const openList = useCallback(() => setView({ kind: "list" }), []);
  const openDetail = useCallback((id: string) => setView({ kind: "detail", id }), []);
  const more = async () => {
    if (!cursor || loading) return;
    setLoading(true);
    try {
      const response = await recordingClient.list(base, cursor);
      if (!response.ok) { setError(response.error.text); return; }
      setRows((old) => [...old, ...response.value.recordings]); setCursor(response.value.next_cursor);
    } catch { setError("More meetings could not be loaded."); }
    finally { setLoading(false); }
  };
  return <div className="recording-workspace">
    {view.kind === "new" ? <Recorder key={view.visit} base={base} title={declaration.title ?? "New recording"} onExit={openList} onWritten={openDetail} /> :
      view.kind === "detail" ? <MinutesDetail key={view.id} base={base} id={view.id} onExit={openList} /> : <>
        <div className="mm-head"><h1>{title}</h1><Button variant="primary" onClick={() => setView({ kind: "new", visit: Date.now() })}>{declaration.label ?? "New recording"}</Button></div>
        <p className="mm-lead">Record a meeting, then read its summary, decisions and follow-ups beside the source transcript.</p>
        <ErrorMessage text={error} />
        {error && <Button onClick={openList}>Retry</Button>}
        {loading && rows.length === 0 ? <p role="status" className="muted">Loading meetings…</p> : rows.length ? <div className="recording-table-wrap"><table className="recording-table"><thead><tr><th>Meeting</th><th>Recorded</th><th>Duration</th><th>Status</th></tr></thead><tbody>
          {rows.map((row) => <tr key={row.id}><td><button className="rowlink" onClick={() => openDetail(row.id)}>{row.title || "Untitled meeting"}</button>{row.participants.length > 0 && <span className="sub">{row.participants.join(", ")}</span>}</td><td>{new Date(row.created_ms).toLocaleString()}</td><td className="num">{clock(row.duration_ms)}</td><td>{row.state === "ready" ? row.has_minutes ? "Minutes ready" : "Transcript saved" : LABEL[row.state]}</td></tr>)}
        </tbody></table></div> : !error && !loading ? <div className="recording-empty"><h2>No meetings yet</h2><p>Start a meeting when the room is ready. Your microphone opens only after you press Record.</p></div> : null}
        {cursor && <Button disabled={loading} onClick={() => void more()}>Load more meetings</Button>}
      </>}
  </div>;
}

function Recorder({ base, title, onExit, onWritten }: { base: RecordingBase; title: string; onExit: () => void; onWritten: (id: string) => void }) {
  const [quote, setQuote] = useState<RecordingQuote | null>(null);
  const [quoteError, setQuoteError] = useState<string | null>(null);
  const [quoteVisit, setQuoteVisit] = useState(0);
  const [name, setName] = useState("");
  const [people, setPeople] = useState("");
  useEffect(() => {
    let current = true;
    setQuote(null); setQuoteError(null);
    void recordingClient.quote(base).then((response) => {
      if (!current) return;
      if (response.ok) setQuote(response.value); else setQuoteError(response.error.text);
    }).catch(() => { if (current) setQuoteError("The recording price could not be checked. Retry before recording."); });
    return () => { current = false; };
  }, [base, quoteVisit]);
  const consent: RecordingConsent | null = useMemo(() => quote && quote.transcription_max_minor !== null && quote.consolidation_max_minor !== null && !quote.unavailable_reason
    ? { transcription_max_minor: quote.transcription_max_minor, consolidation_max_minor: quote.consolidation_max_minor, currency: quote.currency } : null, [quote]);
  const owner = useRecordingSession(base, consent);
  const { state } = owner;
  const recording = state.phase === "recording";
  const processing = ["saving", "finishing", "transcribing", "consolidating"].includes(state.phase);
  const editable = owner.canStart && (state.phase === "ready" || state.phase === "error");
  const note = quoteError ?? quote?.unavailable_reason ?? state.error;
  useEffect(() => { if (state.phase === "ready" && state.recordingId) onWritten(state.recordingId); }, [onWritten, state.phase, state.recordingId]);
  const leave = async () => { if (await owner.leave()) onExit(); };
  return <section aria-label={title}>
    <div className="mm-head"><button type="button" className="back" onClick={() => void leave()}>← Meetings</button><h1>{state.phase === "saving" ? "Saving recording" : processing ? "Consolidating" : title}</h1></div>
    <p className="mm-lead">Make sure everyone knows the meeting is being recorded. Only what is captured goes into the minutes.</p>
    <div className="stage"><div className="deck" data-state={state.phase}>
      <div className="rec"><i aria-hidden="true" /><span role="status">{LABEL[state.phase]}</span></div>
      <div className="rec-time num">{clock(state.elapsedMs)}</div>
      <div className="wave" role="meter" aria-label="Microphone level" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(state.level * 100)}>{[0, 1, 2, 3, 4].map((bar) => <span key={bar} style={{ height: `${6 + bar * 5}px`, opacity: recording && state.level > bar / 5 ? 1 : 0.2 }} />)}</div>
      {processing ? <><ol className="steps"><li>Audio capture stopped.</li><li>{state.pendingSegments ? `${state.pendingSegments} audio batches waiting to save.` : state.phase === "saving" ? "Saving the recording without creating minutes." : "Waiting for verified transcript and minutes."}</li><li>No tasks or messages are created.</li></ol><p className="rec-note">This view updates from the saved recording, not a simulated progress timer.</p></> : <>
        <label className="rec-title">Title<input className="field" maxLength={200} value={name} onChange={(event) => { setName(event.target.value); owner.editTitle(event.target.value); }} onBlur={() => void owner.saveTitle()} disabled={processing || state.phase === "failed"} placeholder="Leave blank to name it later" autoComplete="off" /></label>
        <p className="rec-hint">Blank is fine. After you stop, the minutes are named from what was said. A name you type is kept.</p>
        <label className="rec-title">Who was there<input className="field" maxLength={2000} value={people} onChange={(event) => setPeople(event.target.value)} disabled={!editable} placeholder="Optional" autoComplete="off" /></label>
        <p className="rec-note">{state.phase === "paused" ? "Nothing new is captured while paused. Resume, or stop and write the minutes." : "Speech is written to text in 8-second batches. Stopping saves the final short batch before writing minutes."}</p>
      </>}
      {consent && <p className="rec-hint">Record authorizes up to {formatUsdMinor(consent.transcription_max_minor)} for this whole recording’s transcription and up to {formatUsdMinor(consent.consolidation_max_minor)} for minutes. Saved transcript and minutes stay in this app; audio is sent through the host’s speech service.</p>}
      <ErrorMessage text={note} />
      {!quote && !quoteError && <p className="rec-hint" role="status">Checking recording availability and price…</p>}
      {!consent && (quoteError || quote?.unavailable_reason) && <Button onClick={() => setQuoteVisit((visit) => visit + 1)}>Retry availability</Button>}
      <div className="rec-actions">
        {recording ? <Button onClick={() => void owner.pause()}>Pause</Button> : state.phase === "paused" ? <Button variant="primary" onClick={() => void owner.resume()}>Resume</Button> : <Button variant="primary" disabled={!editable || !consent} onClick={() => void owner.start(name, people)}>Record</Button>}
        <Button disabled={!recording && state.phase !== "paused"} onClick={() => void owner.stopAndWrite()}>Stop and write minutes</Button>
        {(state.phase === "failed" || state.phase === "error") && owner.canRetry && state.detail?.error?.retryable !== false && <Button onClick={() => void owner.retry()}>Retry saved recording</Button>}
        {state.phase === "error" && state.pendingSegments > 0 && <Button variant="danger" onClick={() => { if (window.confirm("Discard audio not yet submitted? Audio already submitted may finish processing. Confirmed transcript will be kept.")) void owner.discard().then(onExit); }}>Discard unsaved audio</Button>}
      </div>
    </div><div className="transcript"><div className="transcript-head"><h2>Transcript</h2><span className="kicker">{state.pendingSegments ? `${state.pendingSegments} pending` : state.detail?.segments.length ? `${state.detail.segments.filter((segment) => segment.state === "confirmed").length} batches saved` : "Waiting"}</span></div>
      <Transcript segments={state.detail?.segments ?? []} />
    </div></div>
  </section>;
}

function MinutesDetail({ base, id, onExit }: { base: RecordingBase; id: string; onExit: () => void }) {
  const [detail, setDetail] = useState<RecordingDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [renaming, setRenaming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [visit, setVisit] = useState(0);
  useEffect(() => {
    let current = true;
    let timer: number | undefined;
    const load = async (resetName: boolean) => {
      try {
        const response = await recordingClient.detail(base, id);
        if (!current) return;
        if (!response.ok) { setError(response.error.text); return; }
        const fresh = response.value.recording;
        setDetail((old) => old && old.segments.length > fresh.segments.length ? {
          ...fresh, segments: [...new Map([...old.segments, ...fresh.segments].map((segment) => [segment.sequence, segment])).values()].sort((a, b) => a.sequence - b.sequence),
          segments_next: old.segments_next,
        } : fresh);
        if (resetName) setName(fresh.title ?? "");
        setError(null);
        if (["finishing", "transcribing", "consolidating"].includes(fresh.state)) timer = window.setTimeout(() => void load(false), 1500);
      } catch { if (current) setError("The meeting could not be loaded. Retry the connection."); }
    };
    void load(true);
    return () => { current = false; window.clearTimeout(timer); };
  }, [base, id, visit]);
  const rename = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const response = await recordingClient.rename(base, id, crypto.randomUUID(), name.trim() || null);
      if (!response.ok) { setError(response.error.text); return; }
      setDetail((old) => old ? { ...response.value.recording, segments: old.segments, segments_next: old.segments_next } : response.value.recording);
      setRenaming(false); setError(null);
    } catch { setError("The title update could not be confirmed. Refresh before trying again."); }
    finally { setBusy(false); }
  };
  const more = async () => {
    if (!detail || detail.segments_next === null || busy) return;
    setBusy(true);
    try {
      const response = await recordingClient.detail(base, id, detail.segments_next);
      if (!response.ok) { setError(response.error.text); return; }
      setDetail((old) => old ? { ...response.value.recording, segments: [...old.segments, ...response.value.recording.segments] } : response.value.recording);
    } catch { setError("More transcript could not be loaded."); }
    finally { setBusy(false); }
  };
  const recover = async () => {
    if (!detail || busy) return;
    setBusy(true);
    try {
      const response = detail.state === "recording"
        ? await recordingClient.finish(base, id, { request_id: crypto.randomUUID(), last_sequence: detail.segment_count - 1 })
        : await recordingClient.retry(base, id, crypto.randomUUID());
      if (!response.ok) { setError(response.error.text); return; }
      setDetail(response.value.recording); setVisit((old) => old + 1);
    } catch { setError("The outcome could not be confirmed. Refresh this saved recording before retrying."); }
    finally { setBusy(false); }
  };
  const sourceLabel = (sources: number[]) => sources.map((sequence) => {
    const segment = detail?.segments.find((item) => item.sequence === sequence);
    return `[${sequence + 1}]${segment ? ` ${clock(segment.start_ms)}` : ""}`;
  }).join(", ");
  const noSpeech = detail?.state === "ready" && detail.segments_next === null && detail.segments.length === detail.segment_count && detail.segments.every((segment) => segment.state === "confirmed" && !segment.text?.trim());
  return <section aria-label="Meeting minutes"><div className="mm-head"><button className="back" onClick={onExit}>← Meetings</button><h1>{detail?.title || "Untitled meeting"}</h1>{detail && <Button onClick={() => setRenaming((value) => !value)}>Rename</Button>}</div>
    <ErrorMessage text={error} />
    {!detail ? <><p role="status" className="muted">{error ? "The meeting is unavailable." : "Loading meeting…"}</p>{error && <Button onClick={() => setVisit((old) => old + 1)}>Retry</Button>}</> : <>
      <p className="mm-lead num">{new Date(detail.created_ms).toLocaleString()} · {clock(detail.duration_ms)} · {LABEL[detail.state]}</p>
      {renaming && <form className="rename" onSubmit={(event) => { event.preventDefault(); void rename(); }}><label className="sr-only" htmlFor="recording-rename">Meeting title</label><input className="field" id="recording-rename" maxLength={200} value={name} onChange={(event) => setName(event.target.value)} /><Button type="submit" disabled={busy} variant="primary">Save title</Button><Button type="button" onClick={() => setRenaming(false)}>Cancel</Button></form>}
      {detail.participants.length > 0 && <div className="people">{detail.participants.map((person, index) => <span className="recording-chip" key={`${index}:${person}`}>{person}</span>)}</div>}
      <ErrorMessage text={detail.error?.text ?? null} />
      {detail.state !== "ready" && <div className="recording-recovery"><p>{detail.state === "recording" ? "This recording was left unfinished. No microphone is running here. You can write minutes from the audio already saved." : detail.state === "failed" ? "This recording needs attention. Confirmed transcript is retained." : "The saved recording is processing. This view updates automatically."}</p>{(detail.state === "recording" || detail.state === "failed" && detail.error?.retryable !== false) && <Button disabled={busy} onClick={() => void recover()}>{detail.state === "recording" ? "Write from saved transcript" : "Retry saved recording"}</Button>}<Button disabled={busy} onClick={() => setVisit((old) => old + 1)}>Refresh status</Button></div>}
      <div className="stack">
        {detail.minutes ? <>
          <section className="block"><h2>Summary</h2><p className="prose">{detail.minutes.summary}</p></section>
          <section className="block"><h2>Decisions</h2>{detail.minutes.decisions.length ? <ul className="decisions">{detail.minutes.decisions.map((decision, index) => <li key={index}><span className="recording-chip">Decision</span><span>{decision.text} <span className="cite">{sourceLabel(decision.sources)}</span></span></li>)}</ul> : <p className="muted">No explicit decisions recorded.</p>}</section>
          <section className="block"><h2>Follow-ups</h2>{detail.minutes.followups.length ? <ul className="decisions">{detail.minutes.followups.map((item, index) => <li key={index}><span className="recording-chip">{item.owner ?? "Unassigned"}</span><span>{item.text}{item.due && <> · {item.due}</>} <span className="cite">{sourceLabel(item.sources)}</span></span></li>)}</ul> : <p className="muted">No explicit follow-ups recorded.</p>}</section>
        </> : detail.state === "ready" ? <section className="block">
          <h2>{noSpeech ? "No speech detected" : "Minutes unavailable"}</h2>
          <p className="prose">{noSpeech ? "No spoken transcript was available, so no minutes were generated." : "No validated minutes are saved. Read the retained source transcript below."}</p>
        </section> : null}
        <section className="block"><h2>Source transcript</h2><Transcript segments={detail.segments} empty="No confirmed speech is saved." />{detail.segments_next !== null && <Button disabled={busy} onClick={() => void more()}>Load more transcript</Button>}</section>
      </div>
    </>}
  </section>;
}
