import { useState } from "react";
import Link from "../../ui/Link";
import { useQuery } from "../../lib/useResource";
import { routePath } from "../../lib/router";
import {
  browserStorage,
  checkLabel,
  missingRequired,
  readNudgeDismissed,
  writeNudgeDismissed,
} from "./checks";
import { setupResource } from "./setupApi";

/**
 * First-run detection on Home: a link to /setup while any required setup
 * check is not ready. Dismissible per browser; it says nothing while the
 * checks load or fail — a tailnet viewer's refused request included
 * (Home must not depend on them) — and on a read-only board.
 */
export default function SetupNudge({ readOnly }: { readOnly: boolean | null }) {
  // A read-only viewer is not the operator on the host: no checks, no
  // link — and nothing is fetched before `/api/meta` says which it is.
  return readOnly === false ? <Nudge /> : null;
}

function Nudge() {
  const state = useQuery(setupResource);
  const [dismissed, setDismissed] = useState(() => readNudgeDismissed(browserStorage()));
  if (dismissed || !state.data) return null;
  const missing = missingRequired(state.data);
  if (missing.length === 0) return null;
  const dismiss = () => {
    writeNudgeDismissed(browserStorage());
    setDismissed(true);
  };
  return (
    <div className="px-4 lg:px-8 pt-4 max-w-[62rem] w-full min-w-0">
      <div
        className="card flex flex-wrap items-center gap-x-3 gap-y-2 px-4 py-3 border-accent/40"
        role="status"
      >
        <span className="min-w-0 flex-1 text-secondary text-ink-300">
          <span className="font-medium text-ink-100">Setup isn't finished</span> — still to do:{" "}
          {missing.map(checkLabel).join(", ")}.
        </span>
        <Link href={routePath({ screen: "setup" })} className="lnk text-secondary font-medium">
          Open setup →
        </Link>
        <button
          type="button"
          onClick={dismiss}
          className="chip py-1 bg-ink-800 text-ink-400 hover:text-ink-100"
          aria-label="dismiss the setup reminder"
        >
          dismiss
        </button>
      </div>
    </div>
  );
}
