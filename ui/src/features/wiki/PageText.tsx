import { useEffect, useRef } from "react";
import { NAVIGATED } from "../../lib/useLocation";
import Md from "../../ui/Md";
import { splitFrontmatter } from "./citations";

/** CAD-1915: the page body, with a leading frontmatter block shown as metadata; the editor preview uses it too. */
export default function PageText({ path, text }: { path: string; text: string }) {
  const { meta, body } = splitFrontmatter(text);
  const marker = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    const headings = [...(marker.current?.parentElement?.querySelectorAll("h1,h2,h3,h4,h5,h6") ?? [])];
    assignHeadingIds(headings);
    const scroll = () => {
      let target = "";
      try { target = decodeURIComponent(location.hash.slice(1)); } catch { /* a malformed fragment targets nothing */ }
      headings.find((heading) => heading.id === ID_PREFIX + slug(target))?.scrollIntoView();
    };
    scroll();
    window.addEventListener("popstate", scroll);
    window.addEventListener(NAVIGATED, scroll);
    return () => {
      window.removeEventListener("popstate", scroll);
      window.removeEventListener(NAVIGATED, scroll);
    };
  }, [body, path]);
  return (
    <>
      <span ref={marker} hidden />
      {meta.length > 0 && (
        <dl className="wk-front">
          {meta.map(([key, value], index) => (
            <div key={index}>
              <dt>{key}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
      )}
      <Md text={body} wikiPage={path} />
    </>
  );
}

// Heading ids: GitHub-style slugs (lowercase, spaces to hyphens, punctuation dropped,
// repeats suffixed -1, -2), prefixed so a page can never claim an id the app uses.
const ID_PREFIX = "user-content-";
const slug = (text: string) => text.trim().toLowerCase().replace(/[^\p{L}\p{N}\s_-]/gu, "").replace(/\s/g, "-");

function assignHeadingIds(headings: Element[]): void {
  const seen = new Map<string, number>();
  for (const heading of headings) {
    const base = slug(heading.textContent ?? "");
    const n = seen.get(base) ?? 0;
    seen.set(base, n + 1);
    heading.id = ID_PREFIX + (n === 0 ? base : `${base}-${n}`);
  }
}
