import { useState } from "react";
import { sdk } from "./store.mjs";
import { fixtures } from "./fixtures.mjs";
const started = new Date().toLocaleString();
export default function SocialContent() {
  const [posts, setPosts] = useState(sdk.read),
    [selected, setSelected] = useState("fixture-1");
  const [source, setSource] = useState(""),
    [brand, setBrand] = useState("No brand context");
  const [caption, setCaption] = useState(posts[0]?.caption || ""),
    [error, setError] = useState("");
  const [tab, setTab] = useState("library"),
    [scenario, setScenario] = useState("normal");
  const post = posts.find((p) => p.id === selected);
  const act = (fn) => {
    try {
      fn();
      setPosts(sdk.read());
      setError("");
    } catch (e) {
      setError(e.message);
    }
  };
  const choose = (p) => {
    setSelected(p.id);
    setCaption(p.caption);
  };
  return (
    <>
      <aside className="dev-banner">
        <strong>Development · Fixtures only</strong>
        <span>
          No agents, credentials, external publishing or production approvals.
          Live backend unavailable.
        </span>
        <small>
          Source base {__APP_DEV_REVISION__} · Live working tree · Started{" "}
          {started}
        </small>
      </aside>
      <div className="workspace">
        <aside className="rail">
          <a href="/" className="wordmark">
            cadence <span>/ apps</span>
          </a>
          <p className="eyebrow">CONTENT STUDIO</p>
          <h1>
            Social
            <br />
            Content
          </h1>
          <p>
            One source.
            <br />A considered draft.
          </p>
          <nav aria-label="App views">
            {["library", "review", "local-outbox"].map((t) => (
              <button
                key={t}
                aria-current={tab === t ? "page" : undefined}
                onClick={() => setTab(t)}
              >
                {t === "local-outbox"
                  ? "Local outbox"
                  : t[0].toUpperCase() + t.slice(1)}{" "}
                <span>
                  {
                    posts.filter((p) => t === "library" || p.status === t)
                      .length
                  }
                </span>
              </button>
            ))}
          </nav>
          <div className="scenario">
            <label htmlFor="scenario">Preview state</label>
            <select
              id="scenario"
              value={scenario}
              onChange={(e) => setScenario(e.target.value)}
            >
              <option value="normal">Working fixtures</option>
              <option value="loading">Loading</option>
              <option value="empty">Empty</option>
              <option value="error">Error</option>
              <option value="readonly">Read-only</option>
            </select>
            <button
              onClick={() =>
                act(() => {
                  sdk.reset(fixtures);
                  setSelected("fixture-1");
                  setCaption(fixtures[0].caption);
                  setScenario("normal");
                })
              }
            >
              Reset fixtures
            </button>
          </div>
        </aside>
        <main>
          <header>
            <p className="eyebrow">SOCIAL CONTENT / {tab.toUpperCase()}</p>
            <h2>
              {tab === "library"
                ? "Make something worth sharing."
                : tab === "review"
                  ? "A pause before the outbox."
                  : "Staged here. Sent nowhere."}
            </h2>
            <p className="muted">
              Local development flow · App-owned source · No mandatory project
            </p>
          </header>
          {scenario === "loading" ? (
            <p role="status">Loading simulated drafts…</p>
          ) : scenario === "error" ? (
            <div role="alert" className="error">
              Simulated API error.{" "}
              <button onClick={() => setScenario("normal")}>
                Retry fixtures
              </button>
            </div>
          ) : scenario === "empty" ? (
            <div className="empty">
              <h3>Your canvas is clear.</h3>
              <p>Create a draft from source text to begin.</p>
              <button onClick={() => setScenario("normal")}>
                Return to fixtures
              </button>
            </div>
          ) : (
            <>
              {scenario === "readonly" && (
                <p className="notice">
                  Read-only preview: local edits are disabled.
                </p>
              )}
              {tab === "library" && (
                <form
                  className="create"
                  onSubmit={(e) => {
                    e.preventDefault();
                    act(() => {
                      const p = sdk.create(source, brand);
                      choose(p);
                      setSource("");
                    });
                  }}
                >
                  <div>
                    <label htmlFor="source">Source text</label>
                    <textarea
                      id="source"
                      name="source"
                      required
                      maxLength={4000}
                      value={source}
                      disabled={scenario === "readonly"}
                      onChange={(e) => setSource(e.target.value)}
                      placeholder="What would you like to share?"
                    />
                  </div>
                  <div>
                    <label htmlFor="brand">
                      Brand context (optional fixture)
                    </label>
                    <select
                      id="brand"
                      value={brand}
                      disabled={scenario === "readonly"}
                      onChange={(e) => setBrand(e.target.value)}
                    >
                      <option>No brand context</option>
                      <option>Harbour studio</option>
                    </select>
                    <p className="muted">
                      Source is copied unchanged. No generated copy or image is
                      claimed.
                    </p>
                    <button
                      className="primary"
                      disabled={scenario === "readonly"}
                      type="submit"
                    >
                      Create local draft →
                    </button>
                  </div>
                </form>
              )}
              {error && (
                <p role="alert" className="error">
                  {error}
                </p>
              )}
              <div className="content">
                <section aria-label="Draft library" className="draft-list">
                  {posts
                    .filter((p) => tab === "library" || p.status === tab)
                    .map((p) => (
                      <button
                        className={`draft ${selected === p.id ? "selected" : ""}`}
                        key={p.id}
                        onClick={() => choose(p)}
                      >
                        <span className="draft-art" aria-hidden="true">
                          SC<span>{String(p.revision).padStart(2, "0")}</span>
                        </span>
                        <span className="draft-meta">
                          <small>
                            {p.brand} · revision {p.revision}
                          </small>
                          <strong>{p.caption.slice(0, 80)}</strong>
                          <span className="status">
                            {p.status === "local-outbox"
                              ? "Local outbox · simulation"
                              : p.status}
                          </span>
                        </span>
                      </button>
                    ))}
                  {posts.filter((p) => tab === "library" || p.status === tab)
                    .length === 0 && (
                    <p className="muted">No drafts in this view.</p>
                  )}
                </section>
                {post && (
                  <section className="editor" aria-label="Draft editor">
                    <div className="editor-head">
                      <h3>Draft / {post.id}</h3>
                      <span className="status">
                        r{post.revision} · {post.status}
                      </span>
                    </div>
                    <label htmlFor="caption">Caption</label>
                    <textarea
                      id="caption"
                      value={caption}
                      maxLength={4000}
                      disabled={scenario === "readonly"}
                      onChange={(e) => setCaption(e.target.value)}
                    />
                    <div className="editor-actions">
                      <button
                        disabled={
                          scenario === "readonly" || caption === post.caption
                        }
                        onClick={() => act(() => sdk.edit(post.id, caption))}
                      >
                        Save revision
                      </button>
                      <button
                        disabled={
                          scenario === "readonly" ||
                          caption !== post.caption ||
                          post.status !== "draft"
                        }
                        onClick={() => act(() => sdk.review(post.id))}
                      >
                        Mark reviewed
                      </button>
                      <button
                        className="primary"
                        disabled={
                          scenario === "readonly" ||
                          caption !== post.caption ||
                          post.status !== "review"
                        }
                        onClick={() => act(() => sdk.stage(post.id))}
                      >
                        Stage locally
                      </button>
                    </div>
                    <p className="muted">
                      Editing invalidates the simulated review. Only the current
                      reviewed revision can enter this fixture outbox.
                    </p>
                    {post.status === "local-outbox" && (
                      <p className="notice" role="status">
                        Simulation recorded · No external receipt · Nothing
                        published.
                      </p>
                    )}
                    <details>
                      <summary>Source & development boundary</summary>
                      <p>{post.source}</p>
                      <p>
                        This facade keeps state in this tab’s memory. Refresh
                        resets fixtures. Production approval and capability
                        checks are not simulated authority.
                      </p>
                    </details>
                  </section>
                )}
              </div>
            </>
          )}
        </main>
      </div>
    </>
  );
}
