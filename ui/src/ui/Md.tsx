import { createContext, isValidElement, useContext, useState, type ReactNode } from "react";
import Markdown, { defaultUrlTransform, type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { citationSpans, isWikiRevisionHref, wikiLinkHref, wikiRevisionHref } from "../features/wiki/citations";
import { useTranslate } from "../lib/localeText";
import Link from "./Link";
import SafeLink from "./SafeLink";
import IdToken from "./IdToken";
import StagedChip, { StagedSourceContext } from "./StagedChip";
import { stagedRefKey } from "../features/decisions/stagedRef";
import { isIdToken, splitIds } from "../features/home/turnText";

// Bare issue ids become `issue:` links as a remark pass over `text` nodes
// only, so code blocks, inline code, existing links and autolinked URLs
// (none of which hold the id as a text node) are never touched.
interface MdNode {
  type: string;
  value?: string;
  url?: string;
  children?: MdNode[];
  data?: { hProperties?: Record<string, unknown> };
}

type CalloutKind = "note" | "tip" | "important" | "warning" | "caution";

const CALLOUT_LABELS: Record<CalloutKind, string> = {
  note: "Note",
  tip: "Tip",
  important: "Important",
  warning: "Warning",
  caution: "Caution",
};

const CALLOUT_MARKER = /^\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\](?:[ \t]*\r?\n|[ \t]+|$)/i;

function stripBlockquotePrefix(line: string): { quotes: number; rest: string } {
  let pos = 0;
  let quotes = 0;
  while (pos < line.length) {
    let spaces = 0;
    while (spaces < 3 && pos + spaces < line.length && line.charCodeAt(pos + spaces) === 0x20) {
      spaces += 1;
    }
    if (pos + spaces >= line.length || line.charCodeAt(pos + spaces) !== 0x3e) break;
    quotes += 1;
    pos += spaces + 1;
    if (pos < line.length && line.charCodeAt(pos) === 0x20) pos += 1;
  }
  return { quotes, rest: line.slice(pos) };
}

function scanFence(rest: string): { ch: "`" | "~"; len: number; after: string } | null {
  let i = 0;
  while (i < 3 && i < rest.length && rest.charCodeAt(i) === 0x20) i += 1;
  const first = rest[i];
  if (first !== "`" && first !== "~") return null;
  let end = i;
  while (end < rest.length && rest[end] === first) end += 1;
  const len = end - i;
  return len >= 3 ? { ch: first, len, after: rest.slice(end) } : null;
}

/** Close an unfinished fenced code block at the end of an in-flight preview so
 *  live streaming Markdown renders as a code surface instead of raw fences.
 *  Presentation-only: stored entries and stream state keep the server's text. */
export function healStreamingMarkdown(text: string): string {
  let open: { ch: "`" | "~"; len: number; quotes: number } | null = null;
  for (const line of text.split(/\r?\n|\u2028|\u2029/)) {
    const { quotes, rest } = stripBlockquotePrefix(line);
    if (open !== null) {
      if (quotes < open.quotes) {
        open = null;
      } else {
        if (quotes === open.quotes) {
          const close = scanFence(rest);
          if (close && close.ch === open.ch && close.len >= open.len && /^[ \t]*$/.test(close.after)) {
            open = null;
          }
        }
        continue;
      }
    }
    const fence = scanFence(rest);
    if (fence && (fence.ch === "~" || !fence.after.includes("`"))) {
      open = { ch: fence.ch, len: fence.len, quotes };
    }
  }
  if (open === null) return text;
  return `${text}${text.endsWith("\n") ? "" : "\n"}${"> ".repeat(open.quotes)}${open.ch.repeat(open.len)}`;
}

function walkCallouts(node: MdNode): void {
  const kids = node.children;
  if (!kids) return;
  if (node.type === "blockquote" && kids[0]?.type === "paragraph") {
    const firstPara = kids[0];
    const firstText = firstPara.children?.[0];
    const match = firstText?.type === "text" && firstText.value ? CALLOUT_MARKER.exec(firstText.value) : null;
    if (match && firstText?.value !== undefined && firstPara.children) {
      const rest = firstText.value.slice(match[0].length);
      if (rest === "") {
        firstPara.children.shift();
        if (firstPara.children[0]?.type === "break") firstPara.children.shift();
      } else {
        firstText.value = rest;
      }
      if (firstPara.children.length === 0) kids.shift();
      const kind = match[1].toLowerCase();
      node.data = {
        ...node.data,
        hProperties: { ...node.data?.hProperties, "data-callout": kind },
      };
    }
  }
  for (const kid of kids) walkCallouts(kid);
}

const remarkCallouts = () => (tree: MdNode) => walkCallouts(tree);

// One id shape for both the linkifier (bounded, global) and the `issue:`
// validator (anchored); digits are capped so a long run is not an id.
const ISSUE_ID_SRC = "[A-Z]{2,6}-\\d{1,9}";
const ISSUE_ID = new RegExp(`\\b${ISSUE_ID_SRC}\\b`, "g");

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

function linkifyCitations(node: MdNode): void {
  const kids = node.children;
  if (!kids || node.type === "link" || node.type === "linkReference") return;
  const out: MdNode[] = [];
  for (const kid of kids) {
    if (kid.type !== "text" || !kid.value) {
      linkifyCitations(kid);
      out.push(kid);
      continue;
    }
    const spans = citationSpans(kid.value);
    if (spans.length === 0) {
      out.push(kid);
      continue;
    }
    let last = 0;
    for (const span of spans) {
      if (span.start > last) out.push({ type: "text", value: kid.value.slice(last, span.start) });
      const href = wikiRevisionHref(span.citation.path, {
        at: span.citation.revision,
        ...(span.citation.section ? { section: span.citation.section } : {}),
        ...(span.citation.page !== undefined ? { page: span.citation.page } : {}),
      });
      out.push({ type: "link", url: `wiki-rev:${href}`, children: [{ type: "text", value: span.label }] });
      last = span.end;
    }
    if (last < kid.value.length) out.push({ type: "text", value: kid.value.slice(last) });
  }
  node.children = out;
}

const remarkWikiCitations = () => (tree: MdNode) => linkifyCitations(tree);

// CAD-2363: in an assistant answer, a long hex identifier in running text
// becomes an inline-code node, which the `code` component paints as a short
// copyable token. Links keep their text; code is never rewritten.
function markIds(node: MdNode): void {
  const kids = node.children;
  if (!kids || node.type === "link" || node.type === "linkReference") return;
  const out: MdNode[] = [];
  for (const kid of kids) {
    if (kid.type !== "text" || !kid.value) {
      markIds(kid);
      out.push(kid);
      continue;
    }
    for (const part of splitIds(kid.value)) out.push(part.id ? { type: "inlineCode", value: part.text } : { type: "text", value: part.text });
  }
  node.children = out;
}
const remarkIdTokens = () => (tree: MdNode) => markIds(tree);

// CAD-1915: a wiki page's `<!-- … -->` notes are source annotations, not
// content. Only html nodes that are nothing but comments are dropped; a
// comment inside a code fence is a `code` node and stays visible.
const COMMENT_ONLY = /^(?:\s*<!--(?:(?!-->)[\s\S])*-->)+\s*$/;
function dropComments(node: MdNode): void {
  if (!node.children) return;
  node.children = node.children.filter((kid) => !(kid.type === "html" && COMMENT_ONLY.test(kid.value ?? "")));
  node.children.forEach(dropComments);
}
const remarkDropComments = () => (tree: MdNode) => dropComments(tree);
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
const WHOLE_ISSUE_ID = new RegExp(`^${ISSUE_ID_SRC}$`);

function plainText(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(plainText).join("");
  return "";
}

/** A fenced block's language and text, read from the `<code>` child that
 *  react-markdown nests inside `<pre>`. */
function codeBlock(children: ReactNode): { lang: string; code: string } {
  const child = Array.isArray(children) ? children[0] : children;
  if (!isValidElement(child)) return { lang: "", code: "" };
  const props = child.props as { className?: string; children?: ReactNode };
  const lang = /(?:^|\s)language-([\w+-]+)/.exec(props.className ?? "")?.[1] ?? "";
  return { lang, code: plainText(props.children) };
}

/** The one fenced-code renderer: language label, Copy, and the code in
 *  the theme's mono face. Highlighting is deliberately absent: adding a
 *  highlighter is a bundle-size decision, not a rendering detail. */
function CodeBlock({ children }: { children?: ReactNode }) {
  const t = useTranslate();
  const { lang, code } = codeBlock(children);
  return (
    <CodeSurface label={lang || t("code")} code={code}>
      {children}
    </CodeSurface>
  );
}

/** One code surface: label, Copy and a scrollable, keyboard-reachable body. */
export function CodeSurface({ label, code, children }: { label: string; code: string; children?: ReactNode }) {
  const t = useTranslate();
  const [copied, setCopied] = useState(false);
  return (
    <div className="codeblock">
      <div className="codeblock-bar">
        <span className="codeblock-lang num">{label}</span>
        <button
          type="button"
          className="lnk codeblock-copy"
          onClick={() => {
            void navigator.clipboard
              ?.writeText(code)
              .then(() => {
                setCopied(true);
                window.setTimeout(() => setCopied(false), 1600);
              })
              .catch(() => undefined);
          }}
        >
          {copied ? t("Copied") : t("Copy")}
        </button>
      </div>
      <pre tabIndex={0} aria-label={label}>{children}</pre>
    </div>
  );
}

function CalloutBlockquote({
  callout,
  children,
}: {
  callout?: string;
  children?: ReactNode;
}) {
  const t = useTranslate();
  const label = callout && Object.hasOwn(CALLOUT_LABELS, callout) ? CALLOUT_LABELS[callout as CalloutKind] : null;
  if (label === null) return <blockquote>{children}</blockquote>;
  return (
    <blockquote data-callout={callout}>
      <div className="md-callout-head">{t(label)}</div>
      {children}
    </blockquote>
  );
}

const InLinkContext = createContext(false);
const handlerComponents = new WeakMap<object, Components>();
let noHandlerComponents: Components | null = null;
// CAD-2363: the same caches for answers that show identifiers as tokens.
const idHandlerComponents = new WeakMap<object, Components>();
let idComponents: Components | null = null;
// CAD-1915: the set for the wiki page being read, so its own links open in
// place. One slot: the view shows one page at a time, and a new page remounts.
let pageComponents: { page: string; components: Components } | null = null;

/** The renderer set is cached per issue handler. Recreating these component
 *  types on every thread update would make React unmount and reinsert old
 *  code, links and images, which a live log would announce as new content. */
/** CAD-2363: inline code that is exactly one identifier is a copy token —
 *  except inside a link, where a button would both copy and navigate.
 *  CAD-2364: one that names a staged write this surface shows is its chip. */
function IdCode({ className, children }: { className?: string; children?: ReactNode }) {
  const linked = useContext(InLinkContext);
  const staged = useContext(StagedSourceContext);
  const plain = <code className={className}>{children}</code>;
  // A fenced block without a language arrives here too, ending in a newline: it stays code.
  if (className || linked || typeof children !== "string" || children.includes("\n")) return plain;
  const fallback = isIdToken(children) ? <IdToken id={children} /> : plain;
  return staged !== null && stagedRefKey(children) !== null ? (
    <StagedChip source={staged} token={children} fallback={fallback} />
  ) : (
    fallback
  );
}

function buildComponents(onOpen?: (id: string) => void, wikiPage?: string, ids = false, knownIssue?: (id: string) => boolean): Components {
  return {
    ...(ids
      ? {
          // Only inline code that is exactly one identifier; block code keeps
          // its `<code>` (CodeBlock reads these same props).
          code: ({ className, children }) => <IdCode className={className}>{children}</IdCode>,
        }
      : {}),
    table: ({ children }) => (
      <div className="md-table-wrap">
        <table>{children}</table>
      </div>
    ),
    blockquote: (props) => (
      <CalloutBlockquote callout={(props as { "data-callout"?: string })["data-callout"]}>
        {props.children}
      </CalloutBlockquote>
    ),
    img: ({ src, alt }) => {
      const linked = useContext(InLinkContext);
      const safe = markdownImageSrc(src);
      if (!safe) return <span>{alt || "Image unavailable"}</span>;
      const image = <img src={safe} alt={alt ?? ""} loading="lazy" decoding="async" />;
      return linked ? (
        image
      ) : (
        <a className="md-image" href={safe} target="_blank" rel="noreferrer">
          {image}
        </a>
      );
    },
    pre: ({ children }) => <CodeBlock>{children}</CodeBlock>,
    a: ({ href, children }) => {
      const issueId = href?.startsWith("issue:") ? href.slice(6) : null;
      const revisionHref = href?.startsWith("wiki-rev:") ? href.slice("wiki-rev:".length) : null;
      const nested = <InLinkContext.Provider value={true}>{children}</InLinkContext.Provider>;
      const pageHref = wikiPage !== undefined && href && issueId === null && revisionHref === null ? wikiLinkHref(wikiPage, href) : null;
      if (pageHref !== null) return <Link className="lnk" href={pageHref}>{nested}</Link>;
      if (revisionHref !== null) {
        return isWikiRevisionHref(revisionHref) ? (
          <Link className="lnk" href={revisionHref}>{nested}</Link>
        ) : (
          <>{children}</>
        );
      }
      return !href ? (
        // react-markdown blanked an unsafe target: show text, not an
        // anchor that would reopen the board in a new tab.
        <>{children}</>
      ) : issueId !== null ? (
        WHOLE_ISSUE_ID.test(issueId) ? (
          onOpen && (!knownIssue || knownIssue(issueId)) ? (
            <button className="lnk num" onClick={() => onOpen(issueId)}>
              {nested}
            </button>
          ) : (
            <span className="num">{children}</span>
          )
        ) : (
          // A malformed id is shown as text: no button, no anchor.
          <>{children}</>
        )
      ) : (
        <SafeLink className="lnk" href={href} encoded>
          {nested}
        </SafeLink>
      );
    },
  };
}

function componentsFor(onOpen?: (id: string) => void, wikiPage?: string, ids = false): Components {
  if (ids) {
    if (!onOpen) return (idComponents ??= buildComponents(undefined, undefined, true));
    const cachedIds = idHandlerComponents.get(onOpen);
    if (cachedIds) return cachedIds;
    const builtIds = buildComponents(onOpen, undefined, true);
    idHandlerComponents.set(onOpen, builtIds);
    return builtIds;
  }
  if (wikiPage !== undefined) {
    if (pageComponents?.page !== wikiPage) pageComponents = { page: wikiPage, components: buildComponents(undefined, wikiPage) };
    return pageComponents.components;
  }
  if (!onOpen) return (noHandlerComponents ??= buildComponents());
  const cached = handlerComponents.get(onOpen);
  if (cached) return cached;
  const built = buildComponents(onOpen);
  handlerComponents.set(onOpen, built);
  return built;
}

// CAD-2387: the known-id variant, cached per (predicate, opener) so a stable
// pair keeps one component set across renders.
const knownHandlerComponents = new WeakMap<object, WeakMap<object, [Components?, Components?]>>();
function knownComponents(onOpen: (id: string) => void, known: (id: string) => boolean, ids: boolean): Components {
  let byOpen = knownHandlerComponents.get(known);
  if (!byOpen) knownHandlerComponents.set(known, (byOpen = new WeakMap()));
  let pair = byOpen.get(onOpen);
  if (!pair) byOpen.set(onOpen, (pair = []));
  const slot = ids ? 1 : 0;
  return (pair[slot] ??= buildComponents(onOpen, undefined, ids, known));
}

const PLUGINS = [remarkGfm, remarkCallouts, remarkWikiCitations, remarkIssueLinks];
const PAGE_PLUGINS = [...PLUGINS, remarkDropComments];
const ID_PLUGINS = [...PLUGINS, remarkIdTokens];

export default function Md({
  text,
  onOpen,
  wikiPage,
  idTokens = false,
  knownIssue,
}: {
  text: string;
  onOpen?: (id: string) => void;
  /** CAD-2387: when set, only an id it accepts becomes a link; others stay plain. */
  knownIssue?: (id: string) => boolean;
  /** CAD-1915: set only by the wiki page view — its links resolve against this path. */
  wikiPage?: string;
  /** CAD-2363: an assistant answer shows long hex identifiers as short copyable tokens. */
  idTokens?: boolean;
}) {
  const components =
    knownIssue && onOpen && wikiPage === undefined
      ? knownComponents(onOpen, knownIssue, idTokens)
      : componentsFor(onOpen, wikiPage, idTokens && wikiPage === undefined);
  return (
    <Markdown
      // GFM (CAD-551): tables, task lists, strikethrough, autolinks —
      // the providers' answers lean on them.
      remarkPlugins={wikiPage !== undefined ? PAGE_PLUGINS : idTokens ? ID_PLUGINS : PLUGINS}
      // `issue:` ids and pinned wiki revisions are ours; everything else
      // takes react-markdown's safe transform (no `javascript:` and friends).
      urlTransform={(url) => (url.startsWith("issue:") || url.startsWith("wiki-rev:") ? url : defaultUrlTransform(url))}
      components={components}
    >
      {text}
    </Markdown>
  );
}
