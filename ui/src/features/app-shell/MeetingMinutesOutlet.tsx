import { useEffect, useRef, useState } from "react";
import Button from "../../ui/Button";
import type { Viewer } from "../projects/work";
import { workspaceApps, type WorkspaceRun } from "../workspace-apps/workspaceApps";
import type { HostScope } from "./hostActions";
import {
  emptySpeechDraft,
  flushSpeech,
  hearSpeech,
  recordAction,
  speechRecognitionCtor,
  speechStamp,
  speechTranscript,
  type SpeechDraft,
  type SpeechSession,
} from "./meetingCapture";
import {
  failureReason,
  meetingLength,
  meetingListOpen,
  meetingStatus,
  meetingTitle,
  meetingWhen,
  minutesGap,
  minutesGapNote,
  minutesLines,
  minutesSections,
  needsMinutesPage,
  reviewLabel,
  rewriteDraft,
  rewriteNote,
  rewriteOffer,
  sentTranscriptLines,
} from "./meetingMinutes";

export default function MeetingMinutesOutlet({
  scope,
  installationTitle,
  view,
  recordId,
  viewer,
  onView,
  onSelect,
  onStarted,
  onEdit,
}: {
  scope: HostScope;
  installationTitle: string;
  view: "list" | "new";
  recordId: string | null;
  viewer: Viewer;
  onView: (view: "list" | "new") => void;
  onSelect: (recordId: string | null) => void;
  /** One query write: list plus the run that just started. */
  onStarted?: (runId: string) => void;
  /** One query write: Record, with no meeting selected. */
  onEdit?: () => void;
}) {
  const [title, setTitle] = useState("");
  const [people, setPeople] = useState("");
  const [transcript, setTranscript] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [rewriteWhy, setRewriteWhy] = useState<string | null>(null);
  const [recording, setRecording] = useState(false);
  const [hearing, setHearing] = useState("");
  const [elapsedMs, setElapsedMs] = useState(0);
  const speechRef = useRef<SpeechSession | null>(null);
  const draftRef = useRef<SpeechDraft>(emptySpeechDraft());
  const baseRef = useRef("");
  const startedRef = useRef(0);
  const speechSupported = speechRecognitionCtor() !== null;
  const [runs, setRuns] = useState<WorkspaceRun[] | null>(null);
  const [runsError, setRunsError] = useState<string | null>(null);
  const [pages, setPages] = useState<Record<string, string | null>>({});
  const loadedArtifact = useRef<Record<string, string>>({});
  const inflightArtifact = useRef<Record<string, string>>({});
  const canWrite = viewer.operator === true && !viewer.readOnly;
  const transcriptReady = transcript.trim().length > 0 || hearing.trim().length > 0;
  const selected = runs?.find((run) => run.id === recordId) ?? null;
  const recordHeadRef = useRef<HTMLHeadingElement | null>(null);
  useEffect(() => {
    if (view === "new") recordHeadRef.current?.focus();
  }, [view]);
  useEffect(() => {
    if (!recording) return;
    const timer = window.setInterval(() => {
      const elapsed = Date.now() - startedRef.current;
      setElapsedMs(elapsed);
      const flushed = flushSpeech(draftRef.current, elapsed, false);
      if (flushed !== draftRef.current) {
        draftRef.current = flushed;
        setTranscript(speechTranscript(baseRef.current, flushed));
        setHearing(flushed.pending);
      }
    }, 250);
    return () => window.clearInterval(timer);
  }, [recording]);
  useEffect(() => () => speechRef.current?.stop(), []);
  function stopRecording(): string {
    const session = speechRef.current;
    if (!session && !recording) return transcript.trim();
    speechRef.current = null;
    session?.stop();
    const elapsed = startedRef.current ? Date.now() - startedRef.current : elapsedMs;
    const flushed = flushSpeech(draftRef.current, elapsed, true);
    draftRef.current = flushed;
    const text = speechTranscript(baseRef.current, flushed);
    setTranscript(text);
    setHearing("");
    setElapsedMs(elapsed);
    setRecording(false);
    return text;
  }
  function beginMinutes(spoken: string) {
    if (!spoken || busy) return;
    const inputs: Record<string, string> = {
      subject: title.trim() || "Untitled meeting",
      transcript: spoken,
    };
    const who = people.trim();
    if (who) inputs.people = who;
    setBusy(true);
    setNotice(null);
    void workspaceApps.startRun({
      install_id: scope.installId,
      workflow: "consolidate",
      inputs,
      request_id: crypto.randomUUID(),
      expected_quotes: {},
      ...(scope.contextId ? { context_id: scope.contextId } : {}),
    }).then((run) => {
      setNotice("Minutes run started. The writer and reviewer come from this installation's team.");
      setRewriteWhy(null);
      setTranscript("");
      if (onStarted) onStarted(run.id);
      else {
        onView("list");
        onSelect(run.id);
      }
    }, (error: unknown) => {
      setNotice(error instanceof Error ? error.message : "The minutes run did not start.");
    }).finally(() => setBusy(false));
  }
  function startRecording() {
    const Ctor = speechRecognitionCtor();
    if (!Ctor) {
      setNotice("This browser has no speech recognition. Paste a transcript instead.");
      return;
    }
    baseRef.current = transcript.trim();
    draftRef.current = emptySpeechDraft();
    startedRef.current = Date.now();
    setElapsedMs(0);
    setHearing("");
    const session = new Ctor();
    session.continuous = true;
    session.interimResults = false;
    session.lang = "en-US";
    session.onresult = (event) => {
      let draft = draftRef.current;
      for (let i = event.resultIndex; i < event.results.length; i += 1) {
        const result = event.results[i];
        if (result?.isFinal) draft = hearSpeech(draft, result[0].transcript);
      }
      draftRef.current = draft;
      setHearing(draft.pending);
    };
    session.onerror = () => setNotice("Speech recognition stopped. Words already written stay in the transcript.");
    session.onend = () => {
      if (speechRef.current === session) {
        try { session.start(); } catch { setNotice("Speech recognition stopped. Words already written stay in the transcript."); }
      }
    };
    speechRef.current = session;
    try {
      session.start();
      setRecording(true);
      setNotice(null);
    } catch {
      speechRef.current = null;
      setNotice("Speech recognition did not start. Paste a transcript instead.");
    }
  }
  useEffect(() => {
    if (view === "new" || !speechRef.current) return;
    stopRecording();
  }, [view]);
  useEffect(() => {
    if (view !== "list" && recordId === null) return;
    const controller = new AbortController();
    workspaceApps.runs(scope.installId, scope.contextId || undefined, controller.signal).then((rows) => {
      if (controller.signal.aborted) return;
      setRuns(rows);
      setRunsError(null);
    }, (error: unknown) => {
      if (controller.signal.aborted) return;
      setRunsError(error instanceof Error ? error.message : "Minutes runs could not be loaded.");
    });
    return () => controller.abort();
  }, [scope.installId, scope.contextId, view, recordId]);
  useEffect(() => {
    const pending = (runs ?? []).filter((run) => {
      const artifact = run.artifacts[0];
      if (!artifact) return false;
      if (loadedArtifact.current[run.id] === artifact.id || inflightArtifact.current[run.id] === artifact.id) return false;
      return needsMinutesPage(run.snapshot.inputs.subject ?? "", run.id === recordId, true);
    });
    if (pending.length === 0) return;
    const controller = new AbortController();
    for (const run of pending) {
      const artifact = run.artifacts[0];
      if (!artifact) continue;
      inflightArtifact.current[run.id] = artifact.id;
      workspaceApps.artifact(artifact.id, controller.signal).then((page) => {
        if (controller.signal.aborted) return;
        loadedArtifact.current[run.id] = artifact.id;
        delete inflightArtifact.current[run.id];
        setPages((current) => ({ ...current, [run.id]: page.text }));
      }, () => {
        if (controller.signal.aborted) return;
        loadedArtifact.current[run.id] = artifact.id;
        delete inflightArtifact.current[run.id];
        setPages((current) => ({ ...current, [run.id]: null }));
      });
    }
    return () => {
      controller.abort();
      for (const run of pending) delete inflightArtifact.current[run.id];
    };
  }, [recordId, runs]);
  const listOpen = meetingListOpen((runs ?? []).map((run) => run.state));
  useEffect(() => {
    if (!listOpen) return;
    const timer = window.setInterval(() => {
      void workspaceApps.runs(scope.installId, scope.contextId || undefined).then((rows) => {
        setRuns(rows);
        setRunsError(null);
      }, () => undefined);
    }, 5000);
    return () => window.clearInterval(timer);
  }, [listOpen, scope.installId, scope.contextId]);
  const selectedPage = selected ? pages[selected.id] : undefined;
  const minutesText = typeof selectedPage === "string" && selectedPage.trim().length > 0 ? selectedPage : null;
  const gap = selected ? minutesGap(selected.state, !!selected.artifacts[0], selectedPage) : null;
  const sections = minutesText ? minutesSections(minutesText) : null;
  const shown = selected ? meetingTitle(selected.snapshot.inputs.subject ?? "", minutesText) : null;
  const latestReview = selected?.reviews[selected.reviews.length - 1];
  const review = reviewLabel(latestReview?.decision);
  const selectedWhen = selected ? meetingWhen(selected.created) : null;
  const capture = recordAction(recording, transcriptReady);
  const selectedLength = meetingLength(selected?.snapshot.inputs.transcript);
  const sentLines = sentTranscriptLines(selected?.snapshot.inputs.transcript);
  const offerRewrite = !!selected && canWrite && rewriteOffer(selected.state, latestReview?.decision, selected.snapshot.inputs.transcript);
  return (
    <div className="app-outlet" data-outlet="meeting-minutes">
      <div className="app-outlet-tabs" role="tablist" aria-label="Meeting minutes">
        <button type="button" role="tab" aria-selected={view === "list"} className="app-outlet-tab" data-on={view === "list" || undefined} onClick={() => onView("list")}>
          Meetings
        </button>
        <button type="button" role="tab" aria-selected={view === "new"} className="app-outlet-tab" data-on={view === "new" || undefined} onClick={() => onView("new")}>
          Record
        </button>
      </div>
      {view === "list" && (
        <section aria-label="Meetings">
          {runsError ? (
            <p className="card px-4 py-3 text-label text-warn" role="alert">{runsError}</p>
          ) : runs === null ? (
            <p className="text-secondary text-ink-400" role="status">Loading minutes…</p>
          ) : runs.length === 0 ? (
            <div className="card px-4 py-5 text-secondary text-ink-400" data-empty="meetings" role="status">
              <p className="font-medium text-ink-200">No meetings yet in {installationTitle}</p>
              <p className="mt-1">Record a meeting, then read the minutes. This list shows consolidate runs only — nothing here is sample data.</p>
            </div>
          ) : (
            <ul className="grid gap-2">
              {runs.map((run) => {
                const page = pages[run.id];
                const named = meetingTitle(run.snapshot.inputs.subject ?? "", typeof page === "string" ? page : null);
                const when = meetingWhen(run.created);
                const length = meetingLength(run.snapshot.inputs.transcript);
                const reviewed = reviewLabel(run.reviews[run.reviews.length - 1]?.decision);
                const status = meetingStatus(run.state, !!run.artifacts[0], page);
                const why = failureReason(run.failure?.reason);
                return (
                  <li key={run.id}>
                    <button type="button" className="lnk text-left" onClick={() => onSelect(run.id)}>
                      {named.title}
                      <span className="num text-ink-400">
                        {when ? ` · ${when}` : ""}
                        {length ? ` · ${length}` : ""}
                        {named.origin ? ` · ${named.origin}` : ""}
                        {` · ${status}`}
                        {reviewed ? ` · ${reviewed}` : ""}
                      </span>
                      {why ? <span className="text-warn"> · {why}</span> : null}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </section>
      )}
      {view === "new" && (
        <section aria-label="New meeting">
          <h3 ref={recordHeadRef} className="text-cardtitle font-medium text-ink-100" tabIndex={-1} data-outlet-heading>
            New meeting
          </h3>
          {!canWrite ? (
            <p className="card px-4 py-3 mt-2 text-label text-ink-400">Read-only view. A verified operator writes minutes from a pasted transcript.</p>
          ) : (
            <form
              className="card px-4 py-4 mt-2 grid gap-2"
              onSubmit={(event) => {
                event.preventDefault();
                beginMinutes(speechRef.current ? stopRecording() : transcript.trim());
              }}
            >
              {rewriteWhy ? <p id="mm-rewrite-why" className="text-label text-ink-200 whitespace-pre-wrap" role="status">{rewriteWhy}</p> : null}
              <label className="text-label text-ink-300" htmlFor="mm-title">Title</label>
              <input id="mm-title" className="wa-input" value={title} onChange={(e) => setTitle(e.target.value)} maxLength={120} autoComplete="off" placeholder="Leave blank to name it later" />
              <label className="text-label text-ink-300" htmlFor="mm-people">Who was there</label>
              <input id="mm-people" className="wa-input" value={people} onChange={(e) => setPeople(e.target.value)} maxLength={200} autoComplete="off" placeholder="Optional" />
              <div className="flex gap-2 items-center">
                <Button
                  id="mm-record"
                  type="button"
                  variant={capture === "write" ? "primary" : "secondary"}
                  disabled={busy || (capture === "record" && !speechSupported)}
                  title={capture === "record" ? (speechSupported ? "Write speech to text every 8 seconds" : "This browser has no speech recognition") : capture === "write" ? "Stop and send the lines heard so far" : "Stop without writing minutes"}
                  onClick={() => {
                    if (capture === "record") {
                      startRecording();
                      return;
                    }
                    const spoken = stopRecording();
                    if (capture === "stop" || !spoken) {
                      setNotice("Nothing was heard. The recording stopped.");
                      return;
                    }
                    beginMinutes(spoken);
                  }}
                >
                  {capture === "write" ? "Stop and write minutes" : capture === "stop" ? "Stop" : "Record speech"}
                </Button>
                <span className="num text-ink-400" id="mm-elapsed">{speechStamp(elapsedMs)}</span>
              </div>
              {recording && hearing ? <p className="text-label text-ink-400" role="status">Hearing: {hearing}</p> : null}
              <label className="text-label text-ink-300" htmlFor="mm-transcript">Transcript</label>
              <textarea id="mm-transcript" className="wa-input" value={transcript} onChange={(e) => setTranscript(e.target.value)} maxLength={8000} rows={5} readOnly={recording} placeholder="Lines appear every 8 seconds, or paste a transcript." />
              <p className="text-micro text-ink-500">Speech is written into the transcript every 8 seconds. Stop and write minutes sends those lines, including words heard since the last line. Leaving Record keeps the words without sending them.</p>
              {notice && <p className="text-label text-ink-200" role="status">{notice}</p>}
              {!recording ? (
                <div>
                  <Button id="mm-write" type="submit" variant="primary" disabled={!transcriptReady || busy} title={transcriptReady ? "Start the minutes workflow" : "Paste a transcript first"}>
                    {busy ? "Writing…" : "Write minutes"}
                  </Button>
                </div>
              ) : null}
            </form>
          )}
        </section>
      )}
      {recordId !== null && (
        <div className="app-outlet-drawer" role="dialog" aria-modal="false" aria-label="Meeting">
          <div className="app-outlet-drawer-head">
            <h3 className="text-cardtitle font-medium text-ink-100">Meeting</h3>
            <Button size="sm" onClick={() => onSelect(null)} aria-label="Close meeting">Close</Button>
          </div>
          {!selected ? (
            <p className="text-secondary text-ink-400">Meeting <span className="num">{recordId}</span> is not stored. The link stays in the URL.</p>
          ) : (
            <div className="grid gap-2">
              <p className="text-secondary text-ink-200">
                {shown?.title}
                {selectedWhen ? <span className="num text-ink-400"> · {selectedWhen}</span> : null}
                {selectedLength ? <span className="num text-ink-400"> · {selectedLength}</span> : null}
                {shown?.origin ? <span className="num text-ink-400"> · {shown.origin}</span> : null}
                <span className="num text-ink-400"> · {meetingStatus(selected.state, !!selected.artifacts[0], selectedPage)}</span>
              </p>
              {selected.snapshot.inputs.people ? <p className="text-label text-ink-400">Who was there: {selected.snapshot.inputs.people}</p> : null}
              {review ? <p className="text-label text-ink-200">{review}</p> : null}
              {latestReview?.rationale ? <p className="text-secondary text-ink-400 whitespace-pre-wrap">{latestReview.rationale}</p> : null}
              {selected.failure?.reason ? <p className="text-label text-warn" role="alert">{selected.failure.reason}</p> : null}
              {offerRewrite ? (
                <div>
                  <Button
                    id="mm-rewrite"
                    type="button"
                    variant="secondary"
                    title="Copy this transcript onto Record. Nothing is sent until you write the minutes."
                    onClick={() => {
                      if (speechRef.current) stopRecording();
                      const draft = rewriteDraft(selected.snapshot.inputs.subject ?? "", selected.snapshot.inputs.people, selected.snapshot.inputs.transcript);
                      setTitle(draft.title);
                      setPeople(draft.people);
                      setTranscript(draft.transcript);
                      setRewriteWhy(rewriteNote(latestReview?.rationale, selected.failure?.reason));
                      setNotice(null);
                      if (onEdit) onEdit();
                      else onView("new");
                    }}
                  >
                    Write again
                  </Button>
                </div>
              ) : null}
              {sections ? (
                sections.map((section) => {
                  const lines = minutesLines(section.heading, section.body);
                  return (
                    <section key={section.heading} className="grid gap-1">
                      <h4 className="text-label text-ink-300">{section.heading}</h4>
                      {lines ? (
                        lines.length === 0 ? <p className="text-secondary text-ink-400">This section is empty.</p> : (
                          <ul className="grid gap-1">
                            {lines.map((line, index) => <li key={`${section.heading}-${index}`} className="text-secondary text-ink-200">{line}</li>)}
                          </ul>
                        )
                      ) : <p className="text-secondary text-ink-200 whitespace-pre-wrap">{section.body}</p>}
                    </section>
                  );
                })
              ) : gap === "loading" ? (
                <p className="text-secondary text-ink-400" role="status">Loading minutes…</p>
              ) : minutesText ? (
                <>
                  <p className="text-secondary text-ink-400" role="status">{minutesGapNote("unshaped", false)}</p>
                  <pre className="text-secondary text-ink-200 whitespace-pre-wrap">{minutesText}</pre>
                </>
              ) : (
                <>
                  <p className="text-secondary text-ink-400" role="status">{minutesGapNote(gap === "unwritten" || gap === "unloadable" || gap === "absent" ? gap : "unready", sentLines.length > 0)}</p>
                  {sentLines.length > 0 ? (
                    <section className="grid gap-1" aria-label="Transcript sent">
                      <h4 className="text-label text-ink-300">Transcript sent</h4>
                      <ul className="grid gap-1">
                        {sentLines.map((line, index) => <li key={`sent-${index}`} className="text-secondary text-ink-200">{line}</li>)}
                      </ul>
                    </section>
                  ) : null}
                </>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
