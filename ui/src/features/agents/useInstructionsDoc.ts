import { useCallback, useEffect, useRef, useState, type MutableRefObject } from "react";
import { api } from "../../lib/api";
import { useTranslate } from "../../lib/localeText";
import type { StoredAgentProfile } from "../../lib/types";
import type { ToastMsg } from "../../ui/Toast";
import {
  checkHosted,
  checkNative,
  saveHosted,
  saveNative,
  savedText,
  type DocDeps,
  type DocKind,
  type DocOutcome,
} from "./instructionsDoc";

const liveDeps: DocDeps = {
  readProfile: async (id) => (await api.agentProfile(id)).profile,
  writeProfile: async (id, body) => (await api.saveAgentProfile(id, body)).profile,
  readNative: async (alias) => (await api.agent(alias)).agent.instructions ?? null,
  writeNative: (alias, text) => api.setAgentInstructions(alias, text),
};

export type DocPhase = "idle" | "saving" | "conflict" | "unconfirmed";

/** Where the caret was when the textarea last lost it, so Preview and a tab
 *  trip give it back. */
export interface DocSelection {
  start: number;
  end: number;
  top: number;
}

/** Everything the Instructions section needs. It lives on the agent page so a
 *  tab trip keeps the draft, the revision it started from and the caret. */
export interface InstructionsDoc {
  /** null: this agent's text is not edited through this document. */
  kind: DocKind | null;
  read: "idle" | "reading" | "failed" | "ready";
  /** The text the board holds, as of the last read or save. */
  served: string | null;
  draft: string | null;
  setDraft: (text: string | null) => void;
  phase: DocPhase;
  /** Hosted only: when and at which revision the stored text last changed. */
  profile: StoredAgentProfile | null;
  /** Native: what the last save said about when the agent reads it. */
  applies: string | null;
  selection: MutableRefObject<DocSelection>;
  retry: () => void;
  save: () => void;
  saveMine: () => void;
  useTheirs: () => void;
  check: () => void;
  discard: () => void;
}

export function useInstructionsDoc(opts: {
  alias: string;
  kind: DocKind | null;
  /** Hosted: the profile id. */
  profileId: string | null;
  /** Native: what the agent detail served. */
  nativeServed: string | null;
  writable: boolean;
  onSaved: () => void;
  toast: (kind: ToastMsg["kind"], text: string) => void;
  /** Substituted by tests. */
  deps?: DocDeps;
}): InstructionsDoc {
  const { alias, kind, profileId, nativeServed, writable, onSaved } = opts;
  const deps = opts.deps ?? liveDeps;
  const t = useTranslate();
  const toast = useCallback((k: ToastMsg["kind"], text: string) => opts.toast(k, t(text)), [opts.toast, t]);
  const [draft, setDraftState] = useState<string | null>(null);
  const [phase, setPhase] = useState<DocPhase>("idle");
  const [profile, setProfile] = useState<StoredAgentProfile | null>(null);
  /** The board's profile as of the last 409: "Save mine" writes at its revision. */
  const [theirs, setTheirs] = useState<StoredAgentProfile | null>(null);
  const [read, setRead] = useState<InstructionsDoc["read"]>("idle");
  const [nativeSaved, setNativeSaved] = useState<string | null>(null);
  const [applies, setApplies] = useState<string | null>(null);
  const [retries, setRetries] = useState(0);
  const selection = useRef<DocSelection>({ start: 0, end: 0, top: 0 });
  const busy = useRef(false);
  /** The exact text a save sent, and the profile it was sent from. A check
   *  judges THIS, never the draft as it stands now. */
  const sent = useRef<{ text: string; base: StoredAgentProfile | null } | null>(null);

  useEffect(() => {
    if (kind !== "hosted" || profileId === null) return;
    let live = true;
    setRead("reading");
    deps
      .readProfile(profileId)
      .then((p) => {
        if (!live) return;
        setProfile(p);
        setRead("ready");
      })
      .catch(() => live && setRead("failed"));
    return () => {
      live = false;
    };
  }, [kind, profileId, retries, deps]);

  // A reload that shows our text means the override has done its job.
  useEffect(() => {
    if (nativeSaved !== null && nativeServed === nativeSaved) setNativeSaved(null);
  }, [nativeServed, nativeSaved]);

  const served = kind === "hosted" ? (profile?.responsibilities ?? null) : (nativeSaved ?? nativeServed);
  const state = kind === null ? "idle" : kind === "hosted" ? read : served === null ? "idle" : "ready";

  // A draft equal to the stored text is not a draft. Whitespace the board will
  // trim stays: dropping it would eat a newline the person just typed.
  useEffect(() => {
    if (served !== null && draft === served && phase === "idle") setDraftState(null);
  }, [served, draft, phase]);

  const setDraft = useCallback((text: string | null) => setDraftState(text), []);

  /** The board's text is ours now: clear the draft and the pending state. */
  const adopt = useCallback(
    (out: Extract<DocOutcome, { kind: "saved" }>) => {
      if (out.profile) {
        setProfile(out.profile);
        setRead("ready");
      } else setNativeSaved(out.text);
      if (out.applies) setApplies(out.applies);
      setTheirs(null);
      setDraftState(null);
      setPhase("idle");
    },
    [],
  );

  const settle = useCallback(
    (out: DocOutcome, text: string, wasCheck: boolean) => {
      if (kind === null) return;
      switch (out.kind) {
        case "saved":
          adopt(out);
          toast("ok", savedText(kind, out.applies, wasCheck, t));
          if (!out.profile) onSaved();
          return;
        case "conflict":
          setTheirs(out.profile);
          setPhase("conflict");
          toast("warn", "Someone else changed this. Your draft is kept.");
          return;
        case "unconfirmed":
          setPhase("unconfirmed");
          toast("warn", wasCheck ? "Still can’t reach the board. Your draft is kept." : "Not confirmed: the connection dropped. Check before saving again.");
          return;
        case "not_saved":
          if (out.profile) setProfile(out.profile);
          setPhase("idle");
          setDraftState(text);
          toast("warn", "It did not save. Your draft is kept.");
          return;
        case "refused":
          setPhase(phase === "conflict" ? "conflict" : "idle");
          toast("err", out.message);
      }
    },
    [kind, adopt, toast, t, onSaved, phase],
  );

  const run = useCallback(
    async (job: () => Promise<DocOutcome>, text: string, base: StoredAgentProfile | null, wasCheck = false) => {
      if (busy.current) return;
      busy.current = true;
      if (!wasCheck) sent.current = { text, base };
      setPhase("saving");
      try {
        settle(await job(), text, wasCheck);
      } finally {
        busy.current = false;
      }
    },
    [settle],
  );

  const text = draft ?? served ?? "";
  const ready = writable && kind !== null && state === "ready";

  const checkJob = (): (() => Promise<DocOutcome>) | null => {
    const s = sent.current;
    if (!s) return null;
    if (kind === "hosted") return s.base ? () => checkHosted(deps, s.base!, s.text) : null;
    return () => checkNative(deps, alias, s.text);
  };

  return {
    kind,
    read: state,
    served,
    draft,
    setDraft,
    phase,
    profile,
    applies,
    selection,
    retry: () => setRetries((n) => n + 1),
    save: () => {
      if (!ready || draft === null || phase !== "idle") return;
      if (kind === "hosted") {
        if (profile) void run(() => saveHosted(deps, profile, draft), draft, profile);
      } else void run(() => saveNative(deps, alias, draft), draft, null);
    },
    saveMine: () => {
      if (!ready || kind !== "hosted" || profileId === null || phase !== "conflict") return;
      if (theirs) {
        void run(() => saveHosted(deps, theirs, text), text, theirs);
        return;
      }
      // Their version was never read: read it now and let the person look
      // before anything is written.
      deps
        .readProfile(profileId)
        .then((p) => {
          setTheirs(p);
          toast("warn", "Their version was read. Save mine again to write over it.");
        })
        .catch(() => toast("err", "Could not read their version, so nothing was saved."));
    },
    useTheirs: () => {
      if (kind !== "hosted" || profileId === null || busy.current) return;
      deps
        .readProfile(profileId)
        .then((p) => {
          setProfile(p);
          setTheirs(null);
          setDraftState(null);
          setPhase("idle");
          toast("ok", "Loaded their version. Your draft is discarded.");
        })
        .catch(() => toast("err", "Could not read their version. Your draft is kept."));
    },
    check: () => {
      const job = ready ? checkJob() : null;
      if (job && sent.current) void run(job, sent.current.text, sent.current.base, true);
    },
    discard: () => {
      if (phase !== "unconfirmed") {
        setDraftState(null);
        setTheirs(null);
        setPhase("idle");
        return;
      }
      // The last save may have landed: read before throwing the text away, and
      // say what the board holds.
      const job = ready ? checkJob() : null;
      if (!job || busy.current) return;
      busy.current = true;
      setPhase("saving");
      void job()
        .then((out) => {
          if (out.kind === "saved") {
            adopt(out);
            toast("ok", `${t("It did save, so the board holds your text.")}`);
            if (!out.profile) onSaved();
          } else if (out.kind === "unconfirmed") {
            setPhase("unconfirmed");
            toast("warn", "Still can’t reach the board, so nothing was discarded.");
          } else {
            if (out.kind === "conflict" || out.kind === "not_saved") if (out.profile) setProfile(out.profile);
            setDraftState(null);
            setPhase("idle");
            toast(
              "ok",
              out.kind === "conflict"
                ? "The board holds a different version, now shown. Your draft is discarded."
                : "It did not save. The board holds the earlier text, now shown.",
            );
          }
        })
        .finally(() => {
          busy.current = false;
        });
    },
  };
}
