import { ApiError, errorMessage } from "./apiError";
import { privateFetch, withPrivateOperation } from "./privateSession";
import { sessionHeaders } from "./sessionKey";
import { objectField } from "./wire";

/**
 * CAD-2255: the hosted workspace roster, read as the source of *verified member
 * subject ids*.
 *
 * A hosted project policy grants access to a subject id (`userId` here), never
 * to a native `users/<handle>` spelling and never to a display name. This read
 * is the only place the settings page learns those ids, so it validates every
 * row and throws on anything unreadable: an empty list from a failed parse
 * would let the editor offer a grant the server would refuse, or hide one the
 * workspace really has.
 */
export interface TeamMember {
  userId: string;
  name: string;
  email?: string;
  role: "owner" | "member";
  joinedAt: string;
}

export async function readTeamMembers(signal?: AbortSignal): Promise<TeamMember[]> {
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch("/api/team/members", { headers: sessionHeaders() }, operation);
    const parsed: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) {
      throw new ApiError(errorMessage(parsed, `${response.status} ${response.statusText}`), response.status);
    }
    const body = objectField(parsed);
    if (!body || !Array.isArray(body.members))
      throw new Error("the member roster is not a list");
    return body.members.map((row) => {
      const member = objectField(row);
      if (
        !member ||
        typeof member.userId !== "string" ||
        member.userId === "" ||
        typeof member.joinedAt !== "string" ||
        (member.role !== "owner" && member.role !== "member")
      )
        throw new Error("a member row is malformed");
      return {
        userId: member.userId,
        name: typeof member.name === "string" ? member.name : "",
        email: typeof member.email === "string" ? member.email : undefined,
        role: member.role,
        joinedAt: member.joinedAt,
      };
    });
  }, signal);
}
