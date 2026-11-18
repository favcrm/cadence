import { useState } from "react";
import { useTranslate } from "../lib/localeText";
import { shortId } from "../features/home/turnText";

/**
 * CAD-2363: a long identifier in an answer, middle-truncated (`7d2e…a873`).
 * The full value is its title and accessible name; a click copies it.
 */
export default function IdToken({ id }: { id: string }) {
  const t = useTranslate();
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="idtok num"
      title={id}
      aria-label={`${t("Copy ID")} ${id}`}
      data-id-token
      onClick={() => {
        void navigator.clipboard
          ?.writeText(id)
          .then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1600);
          })
          .catch(() => undefined);
      }}
    >
      {copied ? t("Copied") : shortId(id)}
    </button>
  );
}
