export interface ToastMsg {
  kind: "ok" | "warn" | "err";
  text: string;
}

const KIND: Record<ToastMsg["kind"], string> = {
  ok: "border-ink-600 text-ink-200",
  warn: "border-warn/50 text-warn",
  err: "border-fail/50 text-fail",
};

/// Bottom-centre write feedback, fixed-position so it never shifts the
/// board layout. `role=status` announces to screen readers.
export default function Toast({ msg }: { msg: ToastMsg | null }) {
  if (!msg) return null;
  return (
    <div
      role="status"
      className={`fixed bottom-5 left-1/2 -translate-x-1/2 z-40 card px-4 py-2.5 num text-label shadow-2xl shadow-black/60 max-w-[80vw] ${KIND[msg.kind]}`}
    >
      {msg.text}
    </div>
  );
}
