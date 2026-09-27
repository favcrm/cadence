import { useState } from "react";
import { sdk } from "./store.mjs";
import { fixtures } from "./fixtures.mjs";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  CubeIcon,
  File01Icon,
  Tick02Icon,
  MailSend01Icon,
  ContrastIcon,
} from "@hugeicons/core-free-icons";
function Glyph({ icon }) {
  return (
    <HugeiconsIcon
      icon={icon}
      size={15}
      strokeWidth={1.7}
      color="currentColor"
      aria-hidden="true"
    />
  );
}
const views = {
  library: { label: "Library", title: "Content library", icon: File01Icon },
  review: {
    label: "Review",
    title: "Drafts awaiting review",
    icon: Tick02Icon,
  },
  "local-outbox": {
    label: "Local outbox",
    title: "Local outbox",
    icon: MailSend01Icon,
  },
};
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
  const [theme, setTheme] = useState(() => {
    try {
      const saved = window.localStorage.getItem("cadence-theme");
      return saved === "light" || saved === "dark" ? saved : "system";
    } catch {
      return "system";
    }
  });
  function cycleTheme() {
    const next = { system: "light", light: "dark", dark: "system" }[theme];
    try {
      if (next === "system") window.localStorage.removeItem("cadence-theme");
      else window.localStorage.setItem("cadence-theme", next);
    } catch {
      /* Storage unavailable: the selection lasts for this page. */
    }
    if (next === "system")
      document.documentElement.removeAttribute("data-theme");
    else document.documentElement.setAttribute("data-theme", next);
    setTheme(next);
  }
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
      <div className="shell app-shell">
        <aside className="side app-side">
          <a href="/" className="brand">
            <Glyph icon={CubeIcon} /> cadence <small>apps</small>
          </a>
          <div className="side-label slabel">Social Content</div>
          <nav className="nav" aria-label="App views">
            {["library", "review", "local-outbox"].map((t) => (
              <button
                key={t}
                className="navlink"
                aria-current={tab === t ? "page" : undefined}
                onClick={() => setTab(t)}
              >
                <Glyph icon={views[t].icon} />
                {views[t].label}
                <span className="chip nav-count">
                  {
                    posts.filter((p) => t === "library" || p.status === t)
                      .length
                  }
                </span>
              </button>
            ))}
          </nav>
          <div className="scenario side-foot">
            <label htmlFor="scenario">Preview state</label>
            <select
              className="field"
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
              className="btn btn-sm"
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
        <div className="main">
          <header className="topbar">
            <div className="crumb">
              cadence / apps / <b>Social Content</b>
            </div>
            <div className="spacer">
              <span className="chip tone b-warn">Fixtures only</span>
              <button
                className="tg"
                onClick={cycleTheme}
                aria-label={`Theme: ${theme}. Change theme`}
                title={`Theme: ${theme}`}
              >
                <Glyph icon={ContrastIcon} />
              </button>
            </div>
          </header>
          <aside className="dev-banner">
            <span className="badge b-warn">Development · Fixtures only</span>
            <span>
              No agents, credentials, external publishing or production
              approvals. Live backend unavailable.
            </span>
            <small className="num">
              Source base {__APP_DEV_REVISION__} · Live working tree · Started{" "}
              {started}
            </small>
          </aside>
          <div className="body no-rail">
            <main className="content app-content">
              <header className="page-head">
                <p className="slabel">SOCIAL CONTENT / {tab.toUpperCase()}</p>
                <h2>{views[tab].title}</h2>
                <p className="muted">
                  Local development flow · App-owned source · No mandatory
                  project
                </p>
              </header>
              <nav className="tabs" aria-label="Content views">
                {["library", "review", "local-outbox"].map((view) => (
                  <button
                    key={view}
                    className={`tab ${tab === view ? "on" : ""}`}
                    aria-current={tab === view ? "page" : undefined}
                    onClick={() => setTab(view)}
                  >
                    {views[view].label}
                  </button>
                ))}
              </nav>
              {scenario === "loading" ? (
                <p role="status">Loading simulated drafts…</p>
              ) : scenario === "error" ? (
                <div role="alert" className="error">
                  Simulated API error.{" "}
                  <button className="btn" onClick={() => setScenario("normal")}>
                    Retry fixtures
                  </button>
                </div>
              ) : scenario === "empty" ? (
                <div className="empty">
                  <h3>Your canvas is clear.</h3>
                  <p>Create a draft from source text to begin.</p>
                  <button className="btn" onClick={() => setScenario("normal")}>
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
                      className="create card pad"
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
                          className="field"
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
                          className="field"
                          id="brand"
                          value={brand}
                          disabled={scenario === "readonly"}
                          onChange={(e) => setBrand(e.target.value)}
                        >
                          <option>No brand context</option>
                          <option>Harbour studio</option>
                        </select>
                        <p className="muted">
                          Source is copied unchanged. No generated copy or image
                          is claimed.
                        </p>
                        <button
                          className="btn btn-primary"
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
                  <div className="draft-workspace">
                    <section aria-label="Draft library" className="draft-list">
                      {posts
                        .filter((p) => tab === "library" || p.status === tab)
                        .map((p) => (
                          <button
                            className={`draft card tcard ${selected === p.id ? "selected" : ""}`}
                            key={p.id}
                            onClick={() => choose(p)}
                          >
                            <span className="draft-art" aria-hidden="true">
                              SC
                              <span>{String(p.revision).padStart(2, "0")}</span>
                            </span>
                            <span className="draft-meta">
                              <small>
                                {p.brand} · revision {p.revision}
                              </small>
                              <strong>{p.caption.slice(0, 80)}</strong>
                              <span className="badge b-acc">
                                {p.status === "local-outbox"
                                  ? "Local outbox · simulation"
                                  : p.status}
                              </span>
                            </span>
                          </button>
                        ))}
                      {posts.filter(
                        (p) => tab === "library" || p.status === tab,
                      ).length === 0 && (
                        <p className="muted">No drafts in this view.</p>
                      )}
                    </section>
                    {post && (
                      <section
                        className="editor card pad"
                        aria-label="Draft editor"
                      >
                        <div className="editor-head">
                          <h3>Draft / {post.id}</h3>
                          <span className="badge b-acc">
                            r{post.revision} · {post.status}
                          </span>
                        </div>
                        <label htmlFor="caption">Caption</label>
                        <textarea
                          className="field"
                          id="caption"
                          value={caption}
                          maxLength={4000}
                          disabled={scenario === "readonly"}
                          onChange={(e) => setCaption(e.target.value)}
                        />
                        <div className="editor-actions">
                          <button
                            className="btn"
                            disabled={
                              scenario === "readonly" ||
                              caption === post.caption
                            }
                            onClick={() =>
                              act(() => sdk.edit(post.id, caption))
                            }
                          >
                            Save revision
                          </button>
                          <button
                            className="btn"
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
                            className="btn btn-primary"
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
                          Editing invalidates the simulated review. Only the
                          current reviewed revision can enter this fixture
                          outbox.
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
                            This facade keeps state in this tab’s memory.
                            Refresh resets fixtures. Production approval and
                            capability checks are not simulated authority.
                          </p>
                        </details>
                      </section>
                    )}
                  </div>
                </>
              )}
            </main>
          </div>
        </div>
      </div>
    </>
  );
}
