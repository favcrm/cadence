import { showProjectChoices } from "../lib/router";
import Link from "./Link";

/**
 * The in-page project filter: a chip row "All · <project> …".
 * Hidden when there is nothing to choose — one project and no selection.
 */
export default function ProjectFilter({
  project,
  projects,
  hrefFor,
}: {
  project: string;
  projects: { key: string }[];
  hrefFor: (key: string) => string;
}) {
  if (!showProjectChoices(projects.length, project)) return null;
  const keys = projects.map((p) => p.key);
  const extra = project !== "all" && !keys.includes(project) ? [project] : [];
  const choices = [
    { key: "all", label: "All" },
    ...[...keys, ...extra].map((key) => ({ key, label: key })),
  ];
  return (
    <nav aria-label="project" className="flex flex-wrap items-center gap-1.5 min-w-0">
      {choices.map((choice, i) => (
        <span key={choice.key} className="inline-flex items-center gap-1.5">
          {i > 0 && (
            <span className="text-ink-600 text-micro" aria-hidden>
              ·
            </span>
          )}
          <Link
            href={hrefFor(choice.key)}
            replace
            aria-current={project === choice.key ? "page" : undefined}
            className={`chip ${
              project === choice.key
                ? "bg-accent/15 text-accent"
                : "bg-ink-800 text-ink-400 hover:text-ink-100"
            }`}
          >
            {choice.label}
          </Link>
        </span>
      ))}
    </nav>
  );
}
