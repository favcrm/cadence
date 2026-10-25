import { useEffect, useState, type ComponentProps } from "react";
import NeedsRail from "../src/features/home/NeedsRail";
import WorkItemDetail from "../src/features/work-items/WorkItemDetail";
import { resources } from "../src/lib/resources";
import { useMaybeResource } from "../src/lib/useResource";
import { homeNeedIssueId, type HomeDecisionOpen } from "../src/lib/workItems";
import { LocaleProvider } from "../src/lib/locale";

type RailProps = Omit<ComponentProps<typeof NeedsRail>, "onOpenDecision">;

/**
 * The App's mount in miniature (CAD-1811): NeedsRail hands its reading item
 * to the one shared work-item detail, exactly as App does. Tests assert the
 * detail's guards here instead of on a rail-local drawer.
 */
export default function RailWithDetail(props: RailProps) {
  const [decision, setDecision] = useState<HomeDecisionOpen | null>(null);
  const id = decision ? homeNeedIssueId(decision.need) : null;
  const state = useMaybeResource(id ? resources.issue(id) : null);
  useEffect(() => {
    if (id) void resources.issue(id).revalidate();
  }, [id]);
  return (
    <LocaleProvider>
      <NeedsRail {...props} onOpenDecision={setDecision} />
      {decision && id && (
        <WorkItemDetail
          key={`${id}:${decision.need.key}`}
          id={id}
          origin="home"
          detail={state?.data?.id === id ? state.data : null}
          readState={state}
          onRetry={() => void resources.issue(id).refresh()}
          href={`/projects/demo/issues/${id}`}
          onClose={() => setDecision(null)}
          readOnly={props.readOnly}
          writeBlock={null}
          onWrite={() => {}}
          onError={() => {}}
          onOpen={() => {}}
          home={{
            open: decision,
            onDone: (_key, text) => setDecision(decision.next(text)),
            onAsk: (_need, lead) => decision.ask(lead),
          }}
        />
      )}
    </LocaleProvider>
  );
}
