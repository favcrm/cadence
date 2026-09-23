export interface ProjectBoundContext {
  project: string;
}

/** A completion may update request state only while its project generation is current. */
export function requestIsCurrent(
  request: number,
  currentRequest: number,
  selectedProject: string,
  requestProject: string,
): boolean {
  return request === currentRequest && selectedProject === requestProject;
}

/** A response may update the view only when both request generation and project match. */
export function responseBelongsToRequest(
  request: number,
  currentRequest: number,
  selectedProject: string,
  responseProject: string,
): boolean {
  return requestIsCurrent(request, currentRequest, selectedProject, responseProject);
}

/** A bundle from a prior project is never rendered during a selection change. */
export function visibleContext<T extends ProjectBoundContext>(
  selectedProject: string,
  context: T | null,
): T | null {
  return context?.project === selectedProject ? context : null;
}
