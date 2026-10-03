import Markdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import { isLoopbackHref } from "./links";

// Bare issue ids become `issue:` links as a remark pass over `text` nodes
// only, so code blocks, inline code, existing links and autolinked URLs
// (none of which hold the id as a text node) are never touched.
interface MdNode {
  type: string;
  value?: string;
  url?: string;
  children?: MdNode[];
}

const ISSUE_ID = /\b[A-Z]{2,6}-\d+\b/g;

function linkifyChildren(node: MdNode): void {
  const kids = node.children;
  if (!kids || node.type === "link" || node.type === "linkReference") return;
  const out: MdNode[] = [];
  for (const kid of kids) {
    if (kid.type !== "text" || !kid.value) {
      linkifyChildren(kid);
      out.push(kid);
      continue;
    }
    let last = 0;
    for (const m of kid.value.matchAll(ISSUE_ID)) {
      if (m.index > last) out.push({ type: "text", value: kid.value.slice(last, m.index) });
      out.push({ type: "link", url: `issue:${m[0]}`, children: [{ type: "text", value: m[0] }] });
      last = m.index + m[0].length;
    }
    out.push(last === 0 ? kid : { type: "text", value: kid.value.slice(last) });
  }
  node.children = out;
}

const remarkIssueLinks = () => (tree: MdNode) => linkifyChildren(tree);

// Agent-written Markdown must not initiate third-party image requests. The
// board permits two CDN families for reviewed Social Content source cards,
// which use their own receipt-bound image component instead of Md.
function markdownImageSrc(src?: string): string | null {
  if (!src || /^[a-z][a-z0-9+.-]*:/i.test(src) || src.startsWith("//")) return null;
  try {
    return new URL(src, "https://cadence.invalid/").origin === "https://cadence.invalid" ? src : null;
  } catch {
    return null;
  }
}

// Only well-formed ids may reach `onOpen`; `issue:` hrefs can come from
// agent-written Markdown, so the remainder is checked, not trusted.
const WHOLE_ISSUE_ID = /^[A-Z]{2,6}-\d+$/;

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
      remarkPlugins={[remarkGfm, remarkIssueLinks]}
      // `issue:` ids are ours; everything else takes react-markdown's
      // safe transform (no `javascript:` and friends).
      urlTransform={(url) => (url.startsWith("issue:") ? url : defaultUrlTransform(url))}
      components={{
        img: ({ src, alt }) => {
          const safe = markdownImageSrc(src);
          return safe ? <img src={safe} alt={alt ?? ""} /> : <span>{alt || "Image unavailable"}</span>;
        },
        a: ({ href, children }) =>
          href?.startsWith("issue:") ? (
            WHOLE_ISSUE_ID.test(href.slice(6)) ? (
              <button
                className="lnk num"
                onClick={() => onOpen?.(href.slice(6))}
              >
                {children}
              </button>
            ) : (
              // A malformed id is shown as text: no button, no anchor.
              <>{children}</>
            )
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
      {text}
    </Markdown>
  );
}
