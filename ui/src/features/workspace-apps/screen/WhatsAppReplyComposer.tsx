import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useLocale } from "../../../lib/locale";
import { onPrivateSessionReset } from "../../../lib/privateSession";
import Button from "../../../ui/Button";
import { object, type recordServices } from "./screenServices";
import { WHATSAPP_REPLY_INSET_MAX, type WhatsAppReplyAvailability, type WhatsAppReplyBounds } from "./screenProtocol";
import "./whatsAppReplyComposer.css";

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
  const [busy, setBusy] = useState(false);
  const [staleRecipient, setStaleRecipient] = useState(false);
  const [attempt, setAttempt] = useState<{ effectId: string | null; outcome: Outcome } | null>(null);
  const running = useRef(false);
  const pointer = useRef(false);
  const input = useRef<HTMLTextAreaElement>(null);
  const dock = useRef<HTMLDivElement>(null);
  const size = useRef({ measured: -1, room: 0, seq: undefined as number | undefined });
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
  const locked = attempt?.outcome === "uncertain";
  const readOnly = busy || locked || availability !== "verified";
  const disabled = readOnly || !text.trim();

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
  }, [bounds, text, recipient]);
  useLayoutEffect(() => { selection.announce(!!text || busy); }, [selection, text, busy]);
  useEffect(() => { selection.announceAvailability?.(availability); }, [selection, availability]);

  async function send() {
    if (disabled || !recipient || running.current || !selection.current()) return;
    const key = receiptKey(selection, recipient);
    const pending = replyRecovery.get(key)?.pending;
    if (pending) { setAttempt({ effectId: pending, outcome: "uncertain" }); return; }
    running.current = true;
    setBusy(true);
    setAttempt(null);
    const args = { provider, conversation_id: selection.conversationId, text };
    let effectId: string | null = null;
    let submitted = false;
    let outcome: Outcome = "unsent";
    try {
      const prepared = await selection.services.call("whatsapp.reply.prepare", args);
      if (!selection.current()) return;
      if (!prepared.ok || !object(prepared.data) || prepared.data.decision !== "prepared" ||
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
      if (!("run" in plan)) return;
      const receipts = replyRecovery.get(key);
      if (receipts?.pending) { effectId = receipts.pending; outcome = "uncertain"; return; }
      // Retain the effect before starting confirm; navigation could win the response race.
      if (receipts) receipts.pending = effectId;
      else replyRecovery.set(key, { pending: effectId, accepted: null });
      // Only reached from the original trusted click or Enter, never a port callback.
      submitted = true;
      const result = await plan.run();
      const state = result.ok && object(result.data) && object(result.data.reply) ? result.data.reply.state : null;
      outcome = state === "failed" || state === "accepted" || state === "sent" ? state : "uncertain";
    } catch {
      if (submitted) outcome = "uncertain";
    } finally {
      running.current = false;
      if (submitted && effectId) settleReceipt(key, effectId, outcome, group);
      if (selection.current()) {
        const mayHaveSent = submitted && (outcome === "sent" || outcome === "accepted" || outcome === "uncertain");
        setAttempt({ effectId, outcome });
        if (mayHaveSent) setText("");
        setBusy(false);
        selection.announce(!mayHaveSent && !!text, submitted);
      }
    }
  }

  async function check() {
    if (!attempt?.effectId || !recipient || running.current || !selection.current()) return;
    running.current = true;
    setBusy(true);
    const { effectId } = attempt;
    const result = await selection.services.call("whatsapp.reply.status", { provider, effect_id: effectId });
    running.current = false;
    if (!selection.current()) return;
    const recorded = recordedState(result.ok && object(result.data) ? result.data.state : null);
    // An inconclusive delivery check cannot un-confirm a known acceptance.
    const outcome = recorded === "uncertain" && attempt.outcome === "accepted" ? "accepted" : recorded;
    settleReceipt(receiptKey(selection, recipient), effectId, outcome, group);
    setAttempt({ effectId, outcome });
    setBusy(false);
    selection.announce(!!text, true);
  }

  const note = staleRecipient ? (zh ? "未有傳送。請清除草稿並重新開啟對話，以再次核對收件人。" : "Nothing sent. Clear the draft and reopen the conversation to verify the recipient again.")
    : attempt?.outcome === "sent" ? (group ? (zh ? "WhatsApp 已接收群組訊息。" : "WhatsApp accepted the group message.") : (zh ? "已確認送達。" : "Delivery confirmed."))
    : attempt?.outcome === "accepted" ? (zh ? "WhatsApp 已接收，但未確認送達。" : "WhatsApp accepted the message; delivery isn’t confirmed.")
    : attempt?.outcome === "uncertain" ? (zh ? "未能確認是否已傳送。請檢查狀態，不要重新傳送。" : "Send not confirmed. Check the status; don’t resend this reply.")
    : attempt ? (zh ? "未傳送。請檢查對話及連線後再試。" : "Not sent. Check the conversation and connection before trying again.") : null;
  const status = note && <div className="wa-reply-status">
    <p role={staleRecipient || attempt?.outcome === "uncertain" || attempt?.outcome === "failed" ? "alert" : "status"}>{note}</p>
    {(locked || attempt?.outcome === "accepted" && !group) && <Button size="sm" disabled={busy} onClick={() => { void check(); }}>{zh ? "檢查狀態" : "Check status"}</Button>}
  </div>;

  return <div ref={dock} className="wa-reply-dock" hidden={hidden} data-positioned={bounds || overlay ? "" : undefined} data-overlay={overlay ? "" : undefined}
    style={bounds ? {
      marginLeft: `${bounds[0] / 100}%`, marginRight: `${(10000 - bounds[1]) / 100}%`,
      // Leave room for the heading and a readable transcript when the keyboard is open.
      maxBlockSize: overlay ? `min(55%, ${WHATSAPP_REPLY_INSET_MAX}px, max(5rem, 100% - 12rem))` : undefined,
    } : undefined}>
    {!overlay && status}
    <form className="app-chat-composer wa-reply-card" aria-label={zh ? "傳送 WhatsApp 回覆" : "Send a WhatsApp reply"} onSubmit={e => e.preventDefault()}>
      {overlay && status}
      {(!overlay || recipient) && <p className="wa-reply-to" role={!recipient && !selection.announceAvailability ? "status" : undefined}>
        {recipient ? <>{zh ? "收件人：" : "To: "}<bdi>{recipient.label || (group ? (zh ? "WhatsApp 群組" : "WhatsApp group") : (zh ? "未有電話號碼" : "Phone not available"))}</bdi>
          {recipient.reference && <> · {zh ? "對話編號：" : "Chat ID: "}<bdi>{recipient.reference}</bdi></>}</>
          : selection.announceAvailability ? (zh ? "WhatsApp 回覆" : "WhatsApp reply")
          : availability === "checking" ? (zh ? "正在核對收件人…" : "Verifying recipient…")
          : (zh ? "此對話暫時無法回覆，你仍可查看訊息。" : "Replies are unavailable for this chat. You can still read messages.")}
      </p>}
      <textarea ref={input} className="app-chat-box" aria-label={zh ? "回覆" : "Reply"}
        placeholder={overlay && availability !== "verified"
          ? (availability === "checking" ? (zh ? "正在核對收件人…" : "Verifying recipient…") : (zh ? "此對話暫時無法回覆" : "Can’t reply in this conversation"))
          : (zh ? "輸入回覆…" : "Write a reply…")}
        value={text} rows={1} maxLength={4096} readOnly={readOnly}
        onChange={e => setText(e.target.value)} onKeyDown={e => {
          if (e.key !== "Enter" || e.shiftKey || e.altKey || e.ctrlKey || e.metaKey || e.nativeEvent.isComposing || e.keyCode === 229) return;
          e.preventDefault();
          if (e.nativeEvent.isTrusted && !e.repeat) void send();
        }} />
      <div className="app-chat-composer-tools wa-reply-tools">
        {text && !busy && !locked && <Button size="sm" variant="ghost" onClick={() => { setText(""); input.current?.focus(); }}>{zh ? "清除草稿" : "Clear draft"}</Button>}
        {verification?.retryable && <Button size="sm" variant="ghost" onClick={() => { setRetry({ bound: verification.bound }); setVerification(undefined); input.current?.focus(); }}>{zh ? "重新核對" : "Retry verification"}</Button>}
        <span className="wa-reply-hint" aria-live="polite">
          {busy ? (zh ? "處理中…" : "Working…") : !readOnly && <span className="wa-reply-shortcut">{zh ? "Enter 傳送 · Shift+Enter 換行" : "Enter to send · Shift+Enter for a new line"}</span>}
        </span>
        <button type="submit" className="wa-reply-send" aria-label={zh ? "傳送" : "Send"} disabled={disabled}
          onPointerDown={e => { pointer.current = e.nativeEvent.isTrusted && e.button === 0; }} onPointerCancel={() => { pointer.current = false; }}
          onClick={e => {
            e.preventDefault();
            const deliberate = e.nativeEvent.isTrusted && (e.detail === 0 || pointer.current);
            pointer.current = false;
            if (deliberate) void send();
          }}>
          <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M12 19V5m-6 6 6-6 6 6" /></svg>
        </button>
      </div>
    </form>
  </div>;
}
