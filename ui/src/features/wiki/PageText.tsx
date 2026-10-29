import Md from "../../ui/Md";
import { splitFrontmatter } from "./citations";

/** CAD-1915: the page body, with a leading frontmatter block shown as metadata; the editor preview uses it too. */
export default function PageText({ path, text }: { path: string; text: string }) {
  const { meta, body } = splitFrontmatter(text);
  return (
    <>
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
