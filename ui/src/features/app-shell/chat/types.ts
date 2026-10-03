/** The App scope a chat send may carry: the route's install plus the
 *  concrete context the shell owns. The server re-proves it on send. */
export interface ChatScope {
  install_id: string;
  context_id: string;
}

export interface ChatBinding {
  scope: ChatScope | null;
  error: string | null;
}
