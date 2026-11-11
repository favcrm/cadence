import type { WriteResp } from "../../lib/api";
import type { AgentsPayload, IssueCard } from "../../lib/types";
import type { StagedWrite } from "../decisions/stagedWrite";
import type { IssueTab } from "./model";

export interface IssuePageProps {
  project: string;
  id: string;
  tab: IssueTab;
  tabHref: (tab: IssueTab) => string;
  issues: IssueCard[];
  agents: AgentsPayload | null;
  readOnly: boolean;
  writeBlock: string | null;
  kickoffBlock: string | null;
  onWrite: (resp: WriteResp, verb: string) => void;
  onError: (e: unknown, verb: string) => void;
  onOpen: (id: string) => void;
  planHref: string;
  onToast: (text: string) => void;
  /** True on hosted workspaces where local daemon `/lane` routes do not exist. */
  hosted?: boolean;
  /** Configured repositories for `project` (`[]` on non-repository projects). */
  projectRepos?: readonly unknown[] | null;
  /** Staged/decided assistant actions from the shared decisions store, when available. */
  stagedActions?: readonly StagedWrite[];
}
