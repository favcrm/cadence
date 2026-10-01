import { type ResourceState } from "../../lib/cache";
import Button from "../../ui/Button";

export function ReadNotice<T>({
  name,
  state,
  retry,
}: {
  name: string;
  state: ResourceState<T>;
  retry: () => void;
}) {
  if (!state.error && !state.inFlight && state.status !== "loading")
    return null;
  const loading = state.inFlight || state.status === "loading";
  return (
    <div
      className="issue-read-notice"
      data-read-state={name}
      role={state.error ? "alert" : "status"}
    >
      <div>
        {state.error ? (
          <>
            <strong>
              {name[0].toUpperCase() + name.slice(1)} could not be loaded.
            </strong>
            <p>
              {state.error}
              {state.data !== null
                ? " Showing the last known information."
                : ""}
            </p>
          </>
        ) : (
          <span>
            {state.data === null ? "Loading" : "Refreshing"} {name}…
          </span>
        )}
      </div>
      {state.error && (
        <Button loading={loading} onClick={retry}>
          Retry {name}
        </Button>
      )}
    </div>
  );
}
