import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useLocale } from "../../../lib/locale";
import { onPrivateSessionReset } from "../../../lib/privateSession";
import Button from "../../../ui/Button";
import { formatMediaBytes, object, type LinkedStagedAttachment, type recordServices } from "./screenServices";
import { LINKED_MEDIA_MAX_BYTES, LINKED_MEDIA_MAX_IMAGE_BYTES, WHATSAPP_REPLY_INSET_MAX, type WhatsAppReplyAvailability, type WhatsAppReplyBounds } from "./screenProtocol";
import "./whatsAppReplyComposer.css";

const ALLOWED_IMAGE_MIMES = new Set(["image/jpeg", "image/jpg", "image/png", "image/webp"]);
const ALLOWED_NON_IMAGE_MIMES = [
  "video/mp4", "video/webm",
  "audio/mpeg", "audio/ogg", "audio/mp4", "audio/aac", "audio/wav", "audio/webm",
  "application/pdf", "text/plain", "text/csv",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation",
];
const ALLOWED_OUTBOUND_MIMES = new Set([
  ...ALLOWED_IMAGE_MIMES,
  ...ALLOWED_NON_IMAGE_MIMES,
  "audio/mp3", "audio/x-m4a", "audio/m4a", "audio/x-wav", "audio/wave",
]);
const ALLOWED_IMAGE_EXTS = new Set([".jpg", ".jpeg", ".png", ".webp"]);
const ALLOWED_NON_IMAGE_EXTS = [
  ".mp4", ".webm", ".mp3", ".ogg", ".m4a", ".aac", ".wav", ".pdf", ".txt", ".csv", ".docx", ".xlsx", ".pptx",
];
const ALLOWED_OUTBOUND_EXTS = new Set([...ALLOWED_IMAGE_EXTS, ...ALLOWED_NON_IMAGE_EXTS]);
const ATTACH_ACCEPT = [
  "image/jpeg", "image/png", "image/webp",
  ...ALLOWED_NON_IMAGE_MIMES,
  [...ALLOWED_IMAGE_EXTS, ...ALLOWED_NON_IMAGE_EXTS].join(","),
].join(",");

function classifyLocalAttachment(file: File): "image" | "other" | null {
  const mime = (file.type || "").split(";", 1)[0]?.trim().toLowerCase() ?? "";
  if (mime !== "") {
    if (!ALLOWED_OUTBOUND_MIMES.has(mime)) return null;
    return ALLOWED_IMAGE_MIMES.has(mime) ? "image" : "other";
  }
  const dot = file.name.lastIndexOf(".");
  const ext = dot > 0 ? file.name.slice(dot).toLowerCase() : "";
  if (!ALLOWED_OUTBOUND_EXTS.has(ext)) return null;
  return ALLOWED_IMAGE_EXTS.has(ext) ? "image" : "other";
}

export interface WhatsAppReplySelection {
  conversationId: string;
  receiptScope: string;
  services: ReturnType<typeof recordServices>;
  current(): boolean;
  announceAvailability?(state: WhatsAppReplyAvailability): void;
  announceInset?(px: number): number | undefined;
  announce(dirty: boolean, refresh?: boolean): void;
}

type Recipient = { workRef: string; label: string; reference: string | null };
type RecipientCheck = { recipient: Recipient; retryable?: never; bound?: never } | { recipient: null; retryable?: boolean; bound?: Recipient };
const sameRecipient = (left: Recipient | null, right: Recipient) =>
  left?.workRef === right.workRef && left.label === right.label;
type Outcome = "unsent" | "failed" | "uncertain" | "accepted" | "sent";
const PHONE = /^\+[1-9]\d{7,14}$/;
const WORK_REF = /^lw_[0-9a-f]{32}_[0-9a-f]{64}$/;
const EFFECT = /^wae_[0-9a-f]{32}$/;
const provider = "linked";
const AUTO_POLL_DELAYS_MS = [500, 1_000, 1_800, 2_700] as const;
const SENT_BANNER_DISMISS_MS = 3_000;
type StatusTone = "sending" | "confirming" | "sent" | "accepted" | "uncertain" | "failed";
// Keep the uncertain attempt and latest unconfirmed acceptance separately,
// so a later failed send can't erase an earlier delivery check.
const replyRecovery = new Map<string, { pending: string | null; accepted: string | null }>();
onPrivateSessionReset(() => replyRecovery.clear());
const receiptKey = (selection: WhatsAppReplySelection, recipient: Recipient) => `${selection.receiptScope}:${recipient.workRef}`;

function settleReceipt(key: string, effectId: string, outcome: Outcome, group: boolean): void {
  const receipts = replyRecovery.get(key);
  if (!receipts || outcome === "uncertain") return;
  if (receipts.pending === effectId) {
    receipts.pending = null;
    if (outcome === "accepted" && !group) receipts.accepted = effectId;
  } else if (receipts.accepted === effectId && outcome !== "accepted") {
    receipts.accepted = null;
  }
  if (!receipts.pending && !receipts.accepted) replyRecovery.delete(key);
}

async function recipientFor(selection: WhatsAppReplySelection, ref = selection.conversationId): Promise<RecipientCheck> {
  if (!selection.current()) return { recipient: null };
  const result = await selection.services.call("whatsapp.conversations.show", { provider, conversation_id: ref });
  if (!result.ok) return { recipient: null, retryable: result.refusal.code === "unavailable" };
  if (!selection.current() || !object(result.data)) return { recipient: null };
  const data = result.data;
  const id = selection.conversationId;
  if (data.contract !== "agenticos.whatsapp-door.v1" || data.conversationId !== id ||
      typeof data.workRef !== "string" || !WORK_REF.test(data.workRef) ||
      !object(data.contact)) return { recipient: null };
  const address = /^([1-9]\d{7,14})(?::\d{1,5})?@s\.whatsapp\.net$/.exec(id);
  const phone = id.endsWith("@g.us") ? null
    : address ? `+${address[1]}`
    : typeof data.contact.phone === "string" && PHONE.test(data.contact.phone) ? data.contact.phone : null;
  const name = typeof data.contact.name === "string" && data.contact.name.length <= 160 ? data.contact.name.trim() : "";
  return { recipient: { workRef: data.workRef, label: [name !== phone ? name : "", phone].filter(Boolean).join(" · "), reference: phone ? null : id } };
}

function recordedState(state: unknown): Outcome {
  // A status read can race a confirmation still in flight. Prepared alone is not a refusal.
  if (state === "expired") return "unsent";
  return state === "sent" || state === "accepted" || state === "failed" ? state : "uncertain";
}

export default function WhatsAppReplyComposer({ selection, bounds, inset }: { selection: WhatsAppReplySelection; bounds?: WhatsAppReplyBounds | null; inset?: { px: number; seq: number } }) {
  const { locale } = useLocale();
  const zh = locale === "zh-TW";
  const [verification, setVerification] = useState<RecipientCheck>();
  const [retry, setRetry] = useState<{ bound?: Recipient }>();
  const [text, setText] = useState("");
  const [pendingOp, setPendingOp] = useState<"send" | "check" | null>(null);
  const busy = pendingOp !== null;
  const sending = pendingOp === "send";
  const checking = pendingOp === "check";
  const [staleRecipient, setStaleRecipient] = useState(false);
  const [attempt, setAttempt] = useState<{ effectId: string | null; outcome: Outcome } | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [attachment, setAttachment] = useState<LinkedStagedAttachment | null>(null);
  const [previewUrl, setPreviewUrl] = useState<string | null>(null);
  const [staging, setStaging] = useState(false);
  const [attachError, setAttachError] = useState<string | null>(null);
  const running = useRef(false);
  const pointer = useRef(false);
  const input = useRef<HTMLTextAreaElement>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const dock = useRef<HTMLDivElement>(null);
  const size = useRef({ measured: -1, room: 0, seq: undefined as number | undefined });
  const stagedIdRef = useRef<string | null>(null);
  const previewUrlRef = useRef<string | null>(null);
  const mountedRef = useRef(true);
  const pollTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const dismissTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const latestSelection = useRef(selection);
  latestSelection.current = selection;

  function clearTimer(ref: { current: ReturnType<typeof setTimeout> | null }) {
    if (ref.current !== null) {
      clearTimeout(ref.current);
      ref.current = null;
    }
  }

  function scheduleSentDismiss(effectId: string | null) {
    clearTimer(dismissTimerRef);
    dismissTimerRef.current = setTimeout(() => {
      dismissTimerRef.current = null;
      if (!mountedRef.current || !latestSelection.current.current()) return;
      setAttempt(curr => (curr?.outcome === "sent" && curr.effectId === effectId ? null : curr));
    }, SENT_BANNER_DISMISS_MS);
  }
  const overlay = selection.announceInset !== undefined;
  const hidden = bounds === null || (overlay && bounds === undefined);
  useLayoutEffect(() => {
    const node = dock.current;
    const announce = selection.announceInset;
    if (!announce || !node) return;
    const state = size.current;
    const publish = () => {
      const height = node.getBoundingClientRect().height;
      const px = Math.min(WHATSAPP_REPLY_INSET_MAX, Math.ceil(height));
      if (px !== state.measured) {
        state.measured = px;
        // A shrink can already be in flight when the next grow arrives.
        // Narrow the paint fence before the frame is allowed to reclaim room.
        state.room = Math.min(state.room, px);
        state.seq = announce(px);
      }
      if (inset && inset.seq === state.seq) state.room = inset.px;
      node.dataset.inset = state.room === 0 ? "empty" : state.room < height ? "partial" : "full";
      node.style.setProperty("--wa-reply-room", `${state.room}px`);
    };
    publish();
    const observer = new ResizeObserver(publish);
    observer.observe(node, { box: "border-box" });
    return () => observer.disconnect();
  }, [selection, inset, hidden]);
  const group = selection.conversationId.endsWith("@g.us");
  const recipient = verification?.recipient;
  const availability = verification === undefined ? "checking" : recipient && !staleRecipient ? "verified" : "blocked";
  const resolvingConfirm = confirming && attempt?.outcome === "uncertain";
  const locked = attempt?.outcome === "uncertain" && !resolvingConfirm;
  const readOnly = busy || resolvingConfirm || locked || availability !== "verified";
  const isAudio = attachment?.kind === "audio";
  const audioHasCaption = isAudio && text !== "";
  const textareaReadOnly = readOnly || (isAudio && text === "");
  const disabled = readOnly || staging || audioHasCaption || (!text.trim() && !attachment);

  function replacePreview(next: string | null) {
    if (previewUrlRef.current && typeof URL.revokeObjectURL === "function") {
      URL.revokeObjectURL(previewUrlRef.current);
    }
    previewUrlRef.current = next;
    setPreviewUrl(next);
  }

  function clearAttachment(deleteServer = true) {
    const currentId = stagedIdRef.current ?? attachment?.attachment_id ?? null;
    stagedIdRef.current = null;
    setAttachment(null);
    setAttachError(null);
    replacePreview(null);
    if (fileInput.current) fileInput.current.value = "";
    if (deleteServer && currentId) {
      void latestSelection.current.services.deleteStagedWhatsAppMedia(currentId);
    }
  }

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      clearTimer(pollTimerRef);
      clearTimer(dismissTimerRef);
      if (previewUrlRef.current && typeof URL.revokeObjectURL === "function") {
        URL.revokeObjectURL(previewUrlRef.current);
      }
      if (stagedIdRef.current) {
        void latestSelection.current.services.deleteStagedWhatsAppMedia(stagedIdRef.current);
      }
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    const bound = retry?.bound;
    void recipientFor(selection, bound?.workRef).then(check => {
      if (cancelled || !selection.current()) return;
      const value = check.recipient;
      if (bound && !sameRecipient(value, bound)) {
        if (check.retryable) setVerification({ ...check, bound });
        else {
          setStaleRecipient(true);
          setVerification({ recipient: bound });
        }
        return;
      }
      // A retry verifies the current draft; do not replace its outcome with an older receipt.
      if (!bound && value) {
        const receipts = replyRecovery.get(receiptKey(selection, value));
        if (receipts?.pending) setAttempt({ effectId: receipts.pending, outcome: "uncertain" });
        else if (receipts?.accepted) setAttempt({ effectId: receipts.accepted, outcome: "accepted" });
      }
      setVerification(check);
    });
    return () => { cancelled = true; };
  }, [selection, retry]);
  useLayoutEffect(() => {
    if (bounds === null) return;
    const box = input.current;
    if (box) {
      box.style.height = "auto";
      box.style.height = `${box.scrollHeight}px`;
    }
  }, [bounds, text, recipient, attachment]);
  useLayoutEffect(() => { selection.announce(!!text || !!attachment || busy || staging); }, [selection, text, attachment, busy, staging]);
  useEffect(() => { selection.announceAvailability?.(availability); }, [selection, availability]);

  async function pickFile(file: File | undefined) {
    if (!file || readOnly || staging || attachment || !selection.current()) return;
    if (fileInput.current) fileInput.current.value = "";
    if (file.size < 1) {
      setAttachError(zh ? "請選擇非空白檔案。" : "Select a non-empty file.");
      return;
    }
    const localKind = classifyLocalAttachment(file);
    if (localKind === null) {
      setAttachError(zh ? "WhatsApp 回覆不支援此檔案格式。" : "That file type is not supported for WhatsApp replies.");
      return;
    }
    if (localKind === "image" && file.size > LINKED_MEDIA_MAX_IMAGE_BYTES) {
      setAttachError(zh ? "圖片大小不得超過 5 MB。" : "Images must be 5 MB or smaller.");
      return;
    }
    if (file.size > LINKED_MEDIA_MAX_BYTES) {
      setAttachError(zh ? "檔案大小不得超過 10 MB。" : "Files must be 10 MB or smaller.");
      return;
    }
    setStaging(true);
    setAttachError(null);
    const res = await selection.services.stageWhatsAppMedia(file);
    if (!selection.current()) {
      if (res.ok) void selection.services.deleteStagedWhatsAppMedia(res.data.attachment_id);
      return;
    }
    if (!res.ok) {
      const code = res.refusal.code;
      setAttachError(
        code === "unsupported_host" ? (zh ? "此工作區主機尚未支援附件上傳。" : "Attachment uploads are not supported on this workspace host yet.")
          : code === "payload_too_large" ? (zh ? "所選檔案超過大小限制。" : "That attachment exceeds the size limit.")
          : code === "unsupported_media_type" ? (zh ? "WhatsApp 回覆不支援此檔案格式。" : "That file type is not supported for WhatsApp replies.")
          : res.refusal.text,
      );
      setStaging(false);
      return;
    }
    if (res.data.kind === "image") {
      const content = await selection.services.readStagedWhatsAppMedia(res.data.attachment_id);
      if (!selection.current()) {
        void selection.services.deleteStagedWhatsAppMedia(res.data.attachment_id);
        return;
      }
      if (content.ok && typeof URL.createObjectURL === "function") {
        replacePreview(URL.createObjectURL(content.blob));
      }
    }
    stagedIdRef.current = res.data.attachment_id;
    setAttachment(res.data);
    setStaging(false);
  }

  function handleStaleAttachment() {
    clearAttachment(false);
    setAttachError(zh ? "附件已過期或變更，請重新附加後再傳送。" : "The attached file expired or changed. Re-attach it before sending.");
  }

  function scheduleAutoStatusCheck(verifiedKey: string, effectId: string, step: number) {
    clearTimer(pollTimerRef);
    const delay = AUTO_POLL_DELAYS_MS[step];
    if (delay === undefined) {
      setConfirming(false);
      return;
    }
    pollTimerRef.current = setTimeout(() => {
      pollTimerRef.current = null;
      void (async () => {
        if (!mountedRef.current || !latestSelection.current.current() || running.current) {
          if (mountedRef.current && latestSelection.current.current()) setConfirming(false);
          return;
        }
        try {
          const res = await latestSelection.current.services.call("whatsapp.reply.status", { provider, effect_id: effectId });
          if (!mountedRef.current || !latestSelection.current.current()) return;
          const recorded = recordedState(res.ok && object(res.data) ? res.data.state : null);
          if (recorded === "sent" || recorded === "accepted" || recorded === "failed") {
            settleReceipt(verifiedKey, effectId, recorded, group);
            setConfirming(false);
            setAttempt({ effectId, outcome: recorded });
            latestSelection.current.announce(false, true);
            if (recorded === "sent") scheduleSentDismiss(effectId);
            return;
          }
        } catch {
          // Bounded poll falls back cleanly to manual check below.
        }
        if (!mountedRef.current || !latestSelection.current.current()) return;
        if (step + 1 < AUTO_POLL_DELAYS_MS.length) {
          scheduleAutoStatusCheck(verifiedKey, effectId, step + 1);
        } else {
          setConfirming(false);
        }
      })();
    }, delay);
  }

  async function send() {
    if (disabled || !recipient || running.current || !selection.current()) return;
    const key = receiptKey(selection, recipient);
    const pending = replyRecovery.get(key)?.pending;
    if (pending) { setAttempt({ effectId: pending, outcome: "uncertain" }); return; }
    clearTimer(pollTimerRef);
    clearTimer(dismissTimerRef);
    setConfirming(false);
    running.current = true;
    setPendingOp("send");
    setAttempt(null);
    setAttachError(null);
    const activeAttachment = attachment;
    const draftText = activeAttachment?.kind === "audio" ? "" : text;
    const args: Record<string, unknown> = {
      provider,
      conversation_id: selection.conversationId,
      text: draftText,
      ...(activeAttachment ? { attachments: [{ attachment_id: activeAttachment.attachment_id, sha256: activeAttachment.sha256 }] } : {}),
    };
    let effectId: string | null = null;
    let submitted = false;
    let staleAttachmentHandled = false;
    let outcome: Outcome = "unsent";
    try {
      const prepared = await selection.services.call("whatsapp.reply.prepare", args);
      if (!selection.current()) return;
      if (!prepared.ok) {
        if (activeAttachment && (prepared.refusal.code === "facts_changed" || prepared.refusal.code === "not_found")) {
          staleAttachmentHandled = true;
          handleStaleAttachment();
        }
        return;
      }
      if (!object(prepared.data) || prepared.data.decision !== "prepared" ||
          typeof prepared.data.effect_id !== "string" || !EFFECT.test(prepared.data.effect_id)) return;
      effectId = prepared.data.effect_id;
      // Re-read the exact account epoch after prepare. Since a re-pair changes
      // workRef, a prepared effect cannot be carried to a different account.
      const [live, plan] = await Promise.all([
        recipientFor(selection, recipient.workRef),
        selection.services.planner("whatsapp.reply.send", { ...args, effect_id: effectId }),
      ]);
      if (!selection.current()) return;
      if (!sameRecipient(live.recipient, recipient)) {
        if (live.retryable) setVerification({ ...live, bound: recipient });
        else setStaleRecipient(true);
        return;
      }
      if (!("run" in plan)) {
        if (activeAttachment && (plan.code === "facts_changed" || plan.code === "not_found")) {
          staleAttachmentHandled = true;
          handleStaleAttachment();
        }
        return;
      }
      const receipts = replyRecovery.get(key);
      if (receipts?.pending) { effectId = receipts.pending; outcome = "uncertain"; return; }
      // Retain the effect before starting confirm; navigation could win the response race.
      if (receipts) receipts.pending = effectId;
      else replyRecovery.set(key, { pending: effectId, accepted: null });
      // Only reached from the original trusted click or Enter, never a port callback.
      submitted = true;
      const result = await plan.run();
      if (!result.ok && activeAttachment && (result.refusal.code === "facts_changed" || result.refusal.code === "not_found")) {
        outcome = "failed";
        staleAttachmentHandled = true;
        handleStaleAttachment();
        return;
      }
      const state = result.ok && object(result.data) && object(result.data.reply) ? result.data.reply.state : null;
      outcome = state === "failed" || state === "accepted" || state === "sent" ? state : "uncertain";
    } catch {
      if (submitted) outcome = "uncertain";
    } finally {
      running.current = false;
      if (submitted && effectId) settleReceipt(key, effectId, outcome, group);
      if (selection.current()) {
        const mayHaveSent = submitted && (outcome === "sent" || outcome === "accepted" || outcome === "uncertain");
        if (!staleAttachmentHandled) setAttempt({ effectId, outcome });
        if (mayHaveSent) {
          setText("");
          if (activeAttachment) {
            stagedIdRef.current = null;
            setAttachment(null);
            replacePreview(null);
          }
        }
        if (submitted && effectId && outcome === "uncertain") {
          setConfirming(true);
          scheduleAutoStatusCheck(key, effectId, 0);
        } else if (outcome === "sent") {
          scheduleSentDismiss(effectId);
        }
        setPendingOp(null);
        selection.announce(!mayHaveSent && (!!text || (!!attachment && !staleAttachmentHandled)), submitted);
      }
    }
  }

  async function check() {
    if (!attempt?.effectId || !recipient || running.current || !selection.current()) return;
    clearTimer(pollTimerRef);
    clearTimer(dismissTimerRef);
    setConfirming(false);
    running.current = true;
    setPendingOp("check");
    const { effectId } = attempt;
    try {
      const result = await selection.services.call("whatsapp.reply.status", { provider, effect_id: effectId });
      if (!selection.current()) return;
      const recorded = recordedState(result.ok && object(result.data) ? result.data.state : null);
      // An inconclusive delivery check cannot un-confirm a known acceptance.
      const outcome = recorded === "uncertain" && attempt.outcome === "accepted" ? "accepted" : recorded;
      settleReceipt(receiptKey(selection, recipient), effectId, outcome, group);
      setAttempt({ effectId, outcome });
      if (outcome === "sent") scheduleSentDismiss(effectId);
      selection.announce(!!text || !!attachment, true);
    } finally {
      running.current = false;
      if (selection.current()) setPendingOp(null);
    }
  }

  const audioNote = audioHasCaption
    ? (zh ? "音訊回覆不可包含說明文字。請清除文字或移除音訊。" : "Audio replies can’t include a caption. Clear the text or remove the audio.")
    : null;
  const sendingNote = sending
    ? (attachment ? (zh ? "正在傳送附件…" : "Sending attachment…") : (zh ? "正在傳送回覆…" : "Sending reply…"))
    : null;
  const note = staleRecipient ? (zh ? "未有傳送。請清除草稿並重新開啟對話，以再次核對收件人。" : "Nothing sent. Clear the draft and reopen the conversation to verify the recipient again.")
    : attachError ?? audioNote ?? sendingNote
    ?? (resolvingConfirm ? (zh ? "正在向 WhatsApp 確認傳送狀態…" : "Confirming send…")
    : attempt?.outcome === "sent" ? (group ? (zh ? "WhatsApp 已接收群組訊息。" : "WhatsApp accepted the group message.") : (zh ? "已確認送達。" : "Delivery confirmed."))
    : attempt?.outcome === "accepted" ? (zh ? "WhatsApp 已接收，等待送達確認。" : "Sent to WhatsApp · Delivery pending.")
    : attempt?.outcome === "uncertain" ? (zh ? "未能確認是否已傳送。請檢查狀態，不要重新傳送。" : "Send not confirmed. Check the status; don’t resend this reply.")
    : attempt ? (zh ? "未傳送。請檢查對話及連線後再試。" : "Not sent. Check the conversation and connection before trying again.") : null);
  const statusTone: StatusTone | undefined = staleRecipient || !!attachError || audioHasCaption
    ? "failed"
    : sending ? "sending"
    : resolvingConfirm ? "confirming"
    : attempt?.outcome === "sent" ? "sent"
    : attempt?.outcome === "accepted" ? "accepted"
    : attempt?.outcome === "uncertain" ? "uncertain"
    : attempt ? "failed" : undefined;
  const statusAlert = staleRecipient || !!attachError || audioHasCaption || (!sending && !resolvingConfirm && (attempt?.outcome === "uncertain" || attempt?.outcome === "failed"));
  const canCheckAction = !attachError && !audioHasCaption && !sending && !resolvingConfirm && (locked || (attempt?.outcome === "accepted" && !group));
  const showStatusSpinner = statusTone === "sending" || statusTone === "confirming";
  const sendSpinning = sending || resolvingConfirm;
  const status = note && <div className="wa-reply-status" data-tone={statusTone}>
    <p role={statusAlert ? "alert" : "status"}>
      {showStatusSpinner && <span className="wa-reply-spinner" aria-hidden="true" />}
      <span>{note}</span>
    </p>
    {canCheckAction && <Button size="sm" variant={attempt?.outcome === "accepted" ? "ghost" : undefined} loading={checking} disabled={busy} onClick={() => { void check(); }}>
      {attempt?.outcome === "accepted" ? (zh ? "重新檢查" : "Recheck") : (zh ? "檢查狀態" : "Check status")}
    </Button>}
  </div>;

  const attBadge = attachment
    ? attachment.kind === "document"
      ? attachment.mime_type === "application/pdf" ? "PDF" : "DOC"
      : attachment.kind.toUpperCase()
    : "";

  return <div ref={dock} className="wa-reply-dock" hidden={hidden} data-positioned={bounds || overlay ? "" : undefined} data-overlay={overlay ? "" : undefined}
    style={bounds ? {
      marginLeft: `${bounds[0] / 100}%`, marginRight: `${(10000 - bounds[1]) / 100}%`,
      // Leave room for the heading and a readable transcript when the keyboard is open.
      maxBlockSize: overlay ? `min(55%, ${WHATSAPP_REPLY_INSET_MAX}px, max(5rem, 100% - 12rem))` : undefined,
    } : undefined}>
    {status}
    <form className="app-chat-composer wa-reply-card" data-locked={locked ? "true" : undefined} aria-label={zh ? "傳送 WhatsApp 回覆" : "Send a WhatsApp reply"} onSubmit={e => e.preventDefault()}>
      {(!overlay || recipient) && <p className="wa-reply-to" role={!recipient && !selection.announceAvailability ? "status" : undefined}>
        {recipient ? <>{zh ? "收件人：" : "To: "}<bdi>{recipient.label || (group ? (zh ? "WhatsApp 群組" : "WhatsApp group") : (zh ? "未有電話號碼" : "Phone not available"))}</bdi>
          {recipient.reference && <> · {zh ? "對話編號：" : "Chat ID: "}<bdi>{recipient.reference}</bdi></>}</>
          : selection.announceAvailability ? (zh ? "WhatsApp 回覆" : "WhatsApp reply")
          : availability === "checking" ? <span className="wa-reply-hint-busy"><span className="wa-reply-spinner" aria-hidden="true" />{zh ? "正在核對收件人…" : "Verifying recipient…"}</span>
          : (zh ? "此對話暫時無法回覆，你仍可查看訊息。" : "Replies are unavailable for this chat. You can still read messages.")}
      </p>}
      {attachment && <div className="wa-reply-attachment">
        {previewUrl
          ? <img className="wa-reply-thumb" src={previewUrl} alt={attachment.file_name ?? attachment.kind} />
          : <span className="wa-reply-badge" aria-hidden="true">{attBadge}</span>}
        <span className="wa-reply-att-meta">
          <span className="wa-reply-att-name">{attachment.file_name ?? (zh ? attachment.kind : attachment.kind)}</span>
          <span className="wa-reply-att-size">{formatMediaBytes(attachment.byte_length)}</span>
        </span>
        {!busy && !resolvingConfirm && !locked && <button type="button" className="wa-reply-att-remove" aria-label={zh ? "移除附件" : "Remove attachment"}
          onClick={() => clearAttachment(true)}>×</button>}
      </div>}
      <textarea ref={input} className="app-chat-box" aria-label={zh ? "回覆" : "Reply"}
        placeholder={isAudio && text === ""
          ? (zh ? "音訊回覆不可包含說明文字" : "Audio replies can’t include a caption")
          : overlay && availability !== "verified"
          ? (availability === "checking" ? (zh ? "正在核對收件人…" : "Verifying recipient…") : (zh ? "此對話暫時無法回覆" : "Can’t reply in this conversation"))
          : (zh ? "輸入回覆…" : "Write a reply…")}
        value={text} rows={1} maxLength={4096} readOnly={textareaReadOnly}
        onChange={e => setText(e.target.value)} onKeyDown={e => {
          if (e.key !== "Enter" || e.shiftKey || e.altKey || e.ctrlKey || e.metaKey || e.nativeEvent.isComposing || e.keyCode === 229) return;
          e.preventDefault();
          if (e.nativeEvent.isTrusted && !e.repeat) void send();
        }} />
      <input ref={fileInput} type="file" hidden accept={ATTACH_ACCEPT}
        onChange={e => { void pickFile(e.target.files?.[0]); }} />
      <div className="app-chat-composer-tools wa-reply-tools">
        <button type="button" className="wa-reply-attach" aria-label={zh ? "附加檔案" : "Attach file"}
          data-loading={staging ? "true" : undefined} aria-busy={staging || undefined}
          disabled={readOnly || staging || !!attachment}
          onClick={() => fileInput.current?.click()}>
          {staging
            ? <span className="wa-reply-spinner" aria-hidden="true" />
            : <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" /></svg>}
        </button>
        {(text || attachment) && !busy && !resolvingConfirm && !locked && !staging && <Button size="sm" variant="ghost" onClick={() => { setText(""); clearAttachment(true); input.current?.focus(); }}>{zh ? "清除草稿" : "Clear draft"}</Button>}
        {verification?.retryable && <Button size="sm" variant="ghost" onClick={() => { setRetry({ bound: verification.bound }); setVerification(undefined); input.current?.focus(); }}>{zh ? "重新核對" : "Retry verification"}</Button>}
        <span className="wa-reply-hint" aria-live="polite">
          {staging ? <span className="wa-reply-hint-busy"><span className="wa-reply-spinner" aria-hidden="true" />{zh ? "正在上傳附件…" : "Uploading attachment…"}</span>
            : checking ? (zh ? "正在檢查狀態…" : "Checking status…")
            : sending || resolvingConfirm ? null
            : !readOnly && <span className="wa-reply-shortcut">{zh ? "Enter 傳送 · Shift+Enter 換行" : "Enter to send · Shift+Enter for a new line"}</span>}
        </span>
        <button type="submit" className="wa-reply-send" aria-label={zh ? "傳送" : "Send"}
          data-loading={sendSpinning ? "true" : undefined} aria-busy={sendSpinning || undefined}
          disabled={disabled}
          onPointerDown={e => { pointer.current = e.nativeEvent.isTrusted && e.button === 0; }} onPointerCancel={() => { pointer.current = false; }}
          onClick={e => {
            e.preventDefault();
            const deliberate = e.nativeEvent.isTrusted && (e.detail === 0 || pointer.current);
            pointer.current = false;
            if (deliberate) void send();
          }}>
          {sendSpinning
            ? <span className="wa-reply-spinner" aria-hidden="true" />
            : <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M12 19V5m-6 6 6-6 6 6" /></svg>}
        </button>
      </div>
    </form>
  </div>;
}
