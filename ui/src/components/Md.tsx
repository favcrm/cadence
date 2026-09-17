import Markdown from "react-markdown";

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
      // `issue:` ids are ours; external hrefs still get rel=noreferrer.
      urlTransform={(url) => url}
      components={{
        a: ({ href, children }) =>
          href?.startsWith("issue:") ? (
            <button
              className="lnk num"
              onClick={() => onOpen?.(href.slice(6))}
            >
              {children}
            </button>
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
