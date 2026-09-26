import Markdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import { isLoopbackHref } from "./links";

// Bare issue ids become `issue:` links before rendering; matches inside
// code spans or existing markdown links are left alone.
function linkify(md: string): string {
  return md.replace(
    /(`[^`\n]*`)|\[[^\]\n]*\]\([^)\n]*\)|\b[A-Z]{2,6}-\d+\b/g,
    (m) => (m[0] === "`" || m[0] === "[" ? m : `[${m}](issue:${m})`),
  );
}

export default function Md({
  text,
  onOpen,
}: {
  text: string;
  onOpen?: (id: string) => void;
}) {
  return (
    <Markdown
      // GFM (CAD-551): tables, task lists, strikethrough, autolinks —
      // the providers' answers lean on them.
      remarkPlugins={[remarkGfm]}
      // `issue:` ids are ours; everything else takes react-markdown's
      // safe transform (no `javascript:` and friends).
      urlTransform={(url) => (url.startsWith("issue:") ? url : defaultUrlTransform(url))}
      components={{
        a: ({ href, children }) =>
          href?.startsWith("issue:") ? (
            <button
              className="lnk num"
              onClick={() => onOpen?.(href.slice(6))}
            >
              {children}
            </button>
          ) : href && isLoopbackHref(href) ? (
            // A link to this machine is shown, never followed (CAD-313).
            <span
              className="text-warn"
              title="A link to this machine — not clickable on the board, so the board's session cookie is never sent to another local server. Copy it if you trust it."
            >
              {children} <code className="num break-all">[{href}]</code>
            </span>
          ) : (
            <a
              className="lnk"
              href={href}
              target="_blank"
              rel="noreferrer"
            >
              {children}
            </a>
          ),
      }}
    >
      {linkify(text)}
    </Markdown>
  );
}
