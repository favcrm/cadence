import { createContext, useContext } from "react";

/** The conversation a thread host (Home, an app pane, the member chat) is
 *  showing. An image block reads its job and its bytes for this one, so a
 *  host that feeds no canvas still loads its images, and a block never
 *  trusts the canvas store's conversation, which can belong to another view.
 *  Null outside a host: the block reads nothing. */
export const ImageConversation = createContext<string | null>(null);

export const useImageConversation = (): string | null => useContext(ImageConversation);
