import ProjectContext from "./ProjectContext";
import type { ProjectContext as ProjectContextPayload } from "../../lib/types";

interface ContextProps {
  project: string;
  context: ProjectContextPayload | null;
  contextLoading: boolean;
  contextError: string | null;
  onRetryContext: () => void;
}

export default function Context({
  project,
  context,
  contextLoading,
  contextError,
  onRetryContext,
}: ContextProps) {
  return (
    <main className="px-4 lg:px-8 pt-6 pb-9 w-full">
      <ProjectContext
        project={project}
        context={context}
        loading={contextLoading}
        error={contextError}
        onRetry={onRetryContext}
      />
    </main>
  );
}
