import { useEffect, useRef, useState, type ComponentProps } from "react";
import NeedsRail from "../src/features/home/NeedsRail";
import WorkItemDetail from "../src/features/work-items/WorkItemDetail";
import { resources } from "../src/lib/resources";
import { useMaybeResource } from "../src/lib/useResource";
import { applyDecisionDone, homeNeedIssueId, type HomeDecisionOpen } from "../src/lib/workItems";
import { restoreDetailFocus } from "../src/features/work-items/focus";
import { LocaleProvider } from "../src/lib/locale";

type RailProps = Omit<ComponentProps<typeof NeedsRail>, "onOpenDecision">;

/**
 * The App's mount in miniature (CAD-1811): NeedsRail hands its reading item
 * to the one shared work-item detail, exactly as App does. Tests assert the
 * detail's guards here instead of on a rail-local drawer.
 */
export default function RailWithDetail(props: RailProps) {
  const [decision, setDecision] = useState<HomeDecisionOpen | null>(null);
  const decisionRef = useRef<HomeDecisionOpen | null>(null);
  decisionRef.current = decision;
  const opener = useRef<Element | null>(null);
  const id = decision ? homeNeedIssueId(decision.need) : null;
  const state = useMaybeResource(id ? resources.issue(id) : null);
  useEffect(() => {
    if (id) void resources.issue(id).revalidate();
  }, [id]);
  const open = (next: HomeDecisionOpen) => {
    if (decisionRef.current === null) opener.current = document.activeElement;
    setDecision(next);
  };
  const close = () => {
    setDecision(null);
    restoreDetailFocus(opener.current);
  };
  return (
    <LocaleProvider>
      <NeedsRail {...props} onOpenDecision={open} />
      {decision && id && (
        <WorkItemDetail
          key={`${id}:${decision.need.key}`}
          id={id}
          origin="home"
          detail={state?.data?.id === id ? state.data : null}
          readState={state}
          onRetry={() => void resources.issue(id).refresh()}
          href={`/projects/demo/issues/${id}`}
          onClose={close}
          readOnly={props.readOnly}
          onOpen={() => {}}
          home={{
            open: decision,
            onDone: (done, _key, text) => {
              const outcome = applyDecisionDone(done, text, { decision: decisionRef.current, issueId: id, origin: "home" });
              if (outcome.kind === "ignored") return;
              if (outcome.kind === "closed") {
                close();
                return;
              }
              setDecision(outcome.next);
            },
            onAsk: (_need, lead) => decision.ask(lead),
          }}
        />
      )}
    </LocaleProvider>
  );
}
