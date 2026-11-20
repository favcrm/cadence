import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useLocale } from "../../../lib/locale";
import { onPrivateSessionReset } from "../../../lib/privateSession";
import Button from "../../../ui/Button";
import { object, type recordServices } from "./screenServices";
import "./whatsAppReplyComposer.css";

export interface WhatsAppReplySelection {
  conversationId: string;
  receiptScope: string;
  services: ReturnType<typeof recordServices>;
  current(): boolean;
  announce(dirty: boolean, refresh?: boolean): void;
}

type Recipient = { workRef: string; label: string; reference: string | null };
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

async function recipientFor(selection: WhatsAppReplySelection, ref = selection.conversationId): Promise<Recipient | null> {
  if (!selection.current()) return null;
  const result = await selection.services.call("whatsapp.conversations.show", { provider, conversation_id: ref });
  if (!result.ok || !selection.current() || !object(result.data)) return null;
  const data = result.data;
  const id = selection.conversationId;
  if (data.contract !== "agenticos.whatsapp-door.v1" || data.conversationId !== id ||
      typeof data.workRef !== "string" || !WORK_REF.test(data.workRef) ||
      !object(data.contact)) return null;
  const address = /^([1-9]\d{7,14})(?::\d{1,5})?@s\.whatsapp\.net$/.exec(id);
  const phone = id.endsWith("@g.us") ? null
    : address ? `+${address[1]}`
    : typeof data.contact.phone === "string" && PHONE.test(data.contact.phone) ? data.contact.phone : null;
  const name = typeof data.contact.name === "string" && data.contact.name.length <= 160 ? data.contact.name.trim() : "";
  return { workRef: data.workRef, label: [name !== phone ? name : "", phone].filter(Boolean).join(" · "), reference: phone ? null : id };
}

function recordedState(state: unknown): Outcome {
  // A status read can race a confirmation still in flight. Prepared alone is not a refusal.
  if (state === "expired") return "unsent";
  return state === "sent" || state === "accepted" || state === "failed" ? state : "uncertain";
}

export default function WhatsAppReplyComposer({ selection }: { selection: WhatsAppReplySelection }) {
  const { locale } = useLocale();
  const zh = locale === "zh-TW";
  const [recipient, setRecipient] = useState<Recipient | null>();
  const [load, setLoad] = useState(0);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [staleRecipient, setStaleRecipient] = useState(false);
  const [attempt, setAttempt] = useState<{ effectId: string | null; outcome: Outcome } | null>(null);
  const running = useRef(false);
  const pointer = useRef(false);
  const input = useRef<HTMLTextAreaElement>(null);
  const group = selection.conversationId.endsWith("@g.us");
  const locked = attempt?.outcome === "uncertain";
  const disabled = busy || locked || staleRecipient || !text.trim() || !recipient;

  useEffect(() => {
    let cancelled = false;
    void recipientFor(selection).then(value => {
      if (cancelled || !selection.current()) return;
      const receipts = value && replyRecovery.get(receiptKey(selection, value));
      if (receipts?.pending) setAttempt({ effectId: receipts.pending, outcome: "uncertain" });
      else if (receipts?.accepted) setAttempt({ effectId: receipts.accepted, outcome: "accepted" });
      setRecipient(value);
    });
    return () => { cancelled = true; };
  }, [selection, load]);
  useLayoutEffect(() => {
    const box = input.current;
    if (box) {
      box.style.height = "auto";
      box.style.height = `${box.scrollHeight}px`;
    }
  }, [text, recipient]);
  useLayoutEffect(() => { selection.announce(!!text || busy); }, [selection, text, busy]);

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
      if (!live || live.workRef !== recipient.workRef || live.label !== recipient.label) {
        setStaleRecipient(true);
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

  return <div className="wa-reply-dock">
    {recipient === undefined ? <p className="wa-reply-status" role="status">{zh ? "正在核對收件人…" : "Verifying recipient…"}</p>
      : !recipient ? <div className="wa-reply-status"><p role="alert">{zh ? "未能核對此對話，無法回覆。" : "Couldn’t verify this conversation. Sending is unavailable."}</p>
        <Button size="sm" onClick={() => { setRecipient(undefined); setLoad(n => n + 1); }}>{zh ? "重試" : "Retry"}</Button></div>
      : <>
        {note && <div className="wa-reply-status">
          <p role={staleRecipient || attempt?.outcome === "uncertain" || attempt?.outcome === "failed" ? "alert" : "status"}>{note}</p>
          {(locked || attempt?.outcome === "accepted" && !group) && <Button size="sm" disabled={busy} onClick={() => { void check(); }}>{zh ? "檢查狀態" : "Check status"}</Button>}
        </div>}
        <form className="app-chat-composer wa-reply-card" aria-label={zh ? "傳送 WhatsApp 回覆" : "Send a WhatsApp reply"} onSubmit={e => e.preventDefault()}>
          <p className="wa-reply-to">{zh ? "收件人：" : "To: "}<bdi>{recipient.label || (group ? (zh ? "WhatsApp 群組" : "WhatsApp group") : (zh ? "未有電話號碼" : "Phone not available"))}</bdi>
            {recipient.reference && <> · {zh ? "對話編號：" : "Chat ID: "}<bdi>{recipient.reference}</bdi></>}</p>
          <textarea ref={input} className="app-chat-box" aria-label={zh ? "回覆" : "Reply"} placeholder={zh ? "輸入回覆…" : "Write a reply…"}
            value={text} rows={1} maxLength={4096} readOnly={busy || locked || staleRecipient}
            onChange={e => setText(e.target.value)} onKeyDown={e => {
              if (e.key !== "Enter" || e.shiftKey || e.altKey || e.ctrlKey || e.metaKey || e.nativeEvent.isComposing || e.keyCode === 229) return;
              e.preventDefault();
              if (e.nativeEvent.isTrusted && !e.repeat) void send();
            }} />
          <div className="app-chat-composer-tools wa-reply-tools">
            {text && !busy && !locked && <Button size="sm" variant="ghost" onClick={() => { setText(""); input.current?.focus(); }}>{zh ? "清除草稿" : "Clear draft"}</Button>}
            <span className="wa-reply-hint" aria-live="polite">{busy ? (zh ? "處理中…" : "Working…") : (zh ? "Enter 傳送 · Shift+Enter 換行" : "Enter to send · Shift+Enter for a new line")}</span>
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
      </>}
  </div>;
}
