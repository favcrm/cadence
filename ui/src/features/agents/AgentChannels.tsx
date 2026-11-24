import { useEffect, useState } from "react";
import { useTranslate } from "../../lib/locale";
import Link from "../../ui/Link";
import { humanAgentRooms, type HumanRoomSummary } from "../threads/humanThreadsClient";

type ChannelsState =
  | { status: "loading"; profileId: string }
  | { status: "ready"; profileId: string; rooms: HumanRoomSummary[] }
  | { status: "error"; profileId: string };

/**
 * Shared non-DM channels that both this hosted agent profile and the current
 * viewer belong to, fetched lazily for the agent overview panel.
 */
export function AgentChannels({
  profileId,
  readRooms = humanAgentRooms,
}: {
  profileId: string;
  readRooms?: (uid: string, signal?: AbortSignal) => Promise<HumanRoomSummary[]>;
}) {
  const t = useTranslate();
  const [state, setState] = useState<ChannelsState>({ status: "loading", profileId });

  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;
    setState({ status: "loading", profileId });
    readRooms(profileId, controller.signal)
      .then((rooms) => {
        if (!cancelled) setState({ status: "ready", profileId, rooms });
      })
      .catch(() => {
        if (!cancelled && !controller.signal.aborted) {
          setState({ status: "error", profileId });
        }
      });
    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [profileId, readRooms]);

  const activeState =
    state.profileId === profileId ? state : { status: "loading" as const, profileId };

  return (
    <section data-work-panel="agent-channels">
      <div className="ap-panel-h">
        <h3>{t("Channels")}</h3>
      </div>
      {activeState.status === "loading" ? (
        <p className="rc-note">{t("Loading channels…")}</p>
      ) : activeState.status === "error" ? (
        <p className="rc-note" role="status">
          {t("Channels could not be loaded.")}
        </p>
      ) : activeState.rooms.length === 0 ? (
        <p className="rc-note">{t("Not in any shared channels with you yet.")}</p>
      ) : (
        <div className="ap-rows">
          {activeState.rooms.map((room) => (
            <div key={room.id} className="ap-row">
              <span>
                <Link className="lnk" href={`/threads?h=${encodeURIComponent(room.id)}`}>
                  # {room.title}
                </Link>
              </span>
              {room.unread > 0 && (
                <small className="num">
                  {t("{count} unread").replace("{count}", String(room.unread))}
                </small>
              )}
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
