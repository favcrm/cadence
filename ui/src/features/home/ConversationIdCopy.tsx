import { useEffect, useState } from "react";
import { useTranslate } from "../../lib/locale";

const hostedConversationId = /^tc_[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/;

export default function ConversationIdCopy({
  id,
  workspaceId,
  supportOnly = false,
}: { id: string; workspaceId?: string; supportOnly?: boolean }) {
  const t = useTranslate();
  const scoped = workspaceId !== undefined && hostedConversationId.test(id);
  const copyLabel = t(scoped ? "Copy support reference" : "Copy ID");
  const preview = id.length > 12 ? `${id.slice(0, 6)}…${id.slice(-4)}` : id;
  const [result, setResult] = useState<{ copied: boolean }>();
  useEffect(() => {
    if (!result) return;
    const timeout = window.setTimeout(() => setResult(undefined), 2000);
    return () => window.clearTimeout(timeout);
  }, [result]);

  if (supportOnly && !scoped) return null;

  return (
    <div className="home-conversation-id relative flex shrink-0 items-center gap-1.5 whitespace-nowrap">
      <span className="hidden sm:inline text-micro text-ink-500">{t("Conversation ID")}:</span>
      <button
        type="button"
        className="lnk num min-h-7 cursor-copy text-micro focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
        aria-label={`${copyLabel}: ${preview}`}
        title={`${copyLabel}: ${id}`}
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(scoped ? `${workspaceId}:${id}` : id);
            setResult({ copied: true });
          } catch {
            setResult({ copied: false });
          }
        }}
      >
        {preview}
      </button>
      <span
        role="status"
        className={result ? "pointer-events-none absolute right-0 top-full z-10 mt-1 rounded border border-ink-700 bg-ink-900 px-2 py-1 text-micro text-ink-200" : "sr-only"}
      >
        {result && (scoped
          ? t(result.copied ? "Support reference copied" : "Could not copy the support reference")
          : t(result.copied ? "Conversation ID copied" : "Could not copy the conversation ID"))}
      </span>
    </div>
  );
}
