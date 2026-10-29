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
      // No hash, an empty one, a malformed one (`#%`) or one with no slug characters: nothing to look up.
      let target = "";
      try { target = slug(decodeURIComponent(location.hash.slice(1))); } catch { return; }
      if (target === "") return;
      headings.find((heading) => heading.id === ID_PREFIX + target)?.scrollIntoView();
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

// Heading ids: GitHub-style slugs (lowercase, spaces to hyphens, punctuation dropped),
// prefixed so a page can never claim an id the app uses. Every id issued is unique: a repeat
// takes the first free -1, -2, ... suffix, so with `Foo`, `Foo`, `Foo-1` the second Foo is
// foo-1 and the literal Foo-1 becomes foo-1-1 (github-slugger's rule). A heading whose slug
// is empty (an emoji only) gets no id: there is nothing a link could name it by.
const ID_PREFIX = "user-content-";
const slug = (text: string) => text.trim().toLowerCase().replace(/[^\p{L}\p{N}\s_-]/gu, "").replace(/\s/g, "-");

function assignHeadingIds(headings: Element[]): void {
  const issued = new Set<string>();
  for (const heading of headings) {
    const base = slug(heading.textContent ?? "");
    if (base === "") {
      heading.removeAttribute("id");
      continue;
    }
    let id = base;
    for (let n = 1; issued.has(id); n++) id = `${base}-${n}`;
    issued.add(id);
    heading.id = ID_PREFIX + id;
  }
}
