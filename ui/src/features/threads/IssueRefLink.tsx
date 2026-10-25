import { createContext, useContext } from "react";
import Link from "../../ui/Link";
import { issuePath } from "../issues/model";
import { opensWorkItem } from "../../lib/workItems";

/** The App's opener for the shared work-item detail; absent in a bare frame. */
export const OpenWorkItem = createContext<((id: string) => void) | null>(null);

/**
 * One issue reference on Threads. It stays a real link (copyable, openable
 * in a new tab) and a plain left click opens the same shared detail every
 * other surface uses; a modified click keeps the canonical issue page.
 */
export function IssueRefLink({ id, project }: { id: string; project: string | null | undefined }) {
  const open = useContext(OpenWorkItem);
  if (!project) return <span className="threads-ref">{id}</span>;
  return (
    <Link
      href={issuePath(project, id)}
      className="threads-ref"
      onClick={(event) => {
        if (!open || !opensWorkItem(event)) return;
        event.preventDefault();
        open(id);
      }}
    >
      {id}
    </Link>
  );
}
