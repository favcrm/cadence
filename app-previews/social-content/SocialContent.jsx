import { useEffect, useRef, useState } from "react";
import { studio } from "./store.mjs";
import { postsForDay } from "./calendar.mjs";
import { fixtureWeek, fixtureTimeZone } from "./fixtures.mjs";
import { HugeiconsIcon } from "@hugeicons/react";
import CubeIcon from "@hugeicons/core-free-icons/CubeIcon";
import File01Icon from "@hugeicons/core-free-icons/File01Icon";
import Tick02Icon from "@hugeicons/core-free-icons/Tick02Icon";
import MailSend01Icon from "@hugeicons/core-free-icons/MailSend01Icon";
import ContrastIcon from "@hugeicons/core-free-icons/ContrastIcon";
import SecondaryViews from "./SecondaryViews.jsx";
import PublishPreview from "./PublishPreview.jsx";

const sections = [
  "Home",
  "Library",
  "Runs",
  "Needs you",
  "Publish",
  "Automations",
  "Workflows",
  "Settings",
];
const lanes = {
  draft: "Drafting",
  review: "In review",
  waiting: "Waiting",
  scheduled: "Scheduled",
  published: "Published",
};
const started = new Date().toLocaleTimeString();
function DayPosts({ date, children }) {
  const strip = useRef(null);
  const [edges, setEdges] = useState({ earlier: false, later: false });
  useEffect(() => {
    const element = strip.current;
    function update() {
      const overflow = element.scrollWidth > element.clientWidth + 1;
      setEdges({
        earlier: overflow && element.scrollLeft > 1,
        later:
          overflow &&
          element.scrollLeft + element.clientWidth < element.scrollWidth - 1,
      });
    }
    update();
    element.addEventListener("scroll", update, { passive: true });
    window.addEventListener("resize", update);
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(update);
    observer?.observe(element);
    return () => {
      element.removeEventListener("scroll", update);
      window.removeEventListener("resize", update);
      observer?.disconnect();
    };
  }, [date, children]);
  function advance(direction) {
    const element = strip.current;
    const card = element.querySelector(".post-card");
    const step =
      (card?.getBoundingClientRect().width || 240) +
      (Number.parseFloat(window.getComputedStyle(element).gap) || 8);
    element.scrollBy({
      left: direction * step,
      behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches
        ? "auto"
        : "smooth",
    });
  }
  return (
    <div className="day-strip">
      <div
        ref={strip}
        className="day-posts"
        tabIndex={0}
        role="region"
        aria-label={`Posts planned for ${date}`}
      >
        {children}
      </div>
      <button
        type="button"
        className="btn ghost day-scroll-arrow earlier"
        hidden={!edges.earlier}
        disabled={!edges.earlier}
        aria-label={`Scroll earlier posts for ${date}`}
        onClick={() => advance(-1)}
      >
        ‹
      </button>
      <button
        type="button"
        className="btn ghost day-scroll-arrow later"
        hidden={!edges.later}
        disabled={!edges.later}
        aria-label={`Scroll later posts for ${date}`}
        onClick={() => advance(1)}
      >
        ›
      </button>
    </div>
  );
}
function BoardLane({ label, posts, renderCard }) {
  return (
    <section className="board-lane">
      <header className="row">
        <h3>{label}</h3>
        <span className="chip tone num">{posts.length}</span>
      </header>
      <div
        className="board-lane-body"
        role="region"
        tabIndex={0}
        aria-label={`${label} posts`}
      >
        {posts.length ? (
          posts.map(renderCard)
        ) : (
          <p className="board-empty muted">
            No posts in {label.toLowerCase()}.
          </p>
        )}
      </div>
    </section>
  );
}
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
function Media({ name, className = "" }) {
  return (
    <div className={`fixture-media ${className}`}>
      <Glyph icon={File01Icon} />
      <strong>{name || "No media"}</strong>
      <span>Fixture placeholder</span>
    </div>
  );
}
function Badge({ post }) {
  return (
    <span className={`badge status-${post.status}`}>
      {post.needsYou ? "Needs you" : lanes[post.status]}
      {post.status === "scheduled" || post.status === "published"
        ? " · fixture"
        : ""}
    </span>
  );
}
function Drawer({ title, children, onClose, wide = false }) {
  const panel = useRef(null);
  useEffect(() => {
    const previous = document.activeElement;
    panel.current.querySelector("button")?.focus();
    function key(event) {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      }
      if (event.key !== "Tab") return;
      const nodes = [
        ...panel.current.querySelectorAll(
          "button:not(:disabled),input:not(:disabled),select:not(:disabled),textarea:not(:disabled),a[href],summary",
        ),
      ];
      const first = nodes[0],
        last = nodes.at(-1);
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last?.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first?.focus();
      }
    }
    document.addEventListener("keydown", key);
    return () => {
      document.removeEventListener("keydown", key);
      previous?.focus();
    };
  }, []);
  return (
    <div
      className="drawer-scrim"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <section
        className={`app-drawer ${wide ? "wide" : ""}`}
        ref={panel}
        role="dialog"
        aria-modal="true"
        aria-label={title}
      >
        <header className="drawer-head">
          <h2>{title}</h2>
          <button
            className="btn btn-sm"
            onClick={onClose}
            aria-label={`Close ${title}`}
          >
            Close
          </button>
        </header>
        {children}
      </section>
    </div>
  );
}
function SourceDrawer({ source, posts, onClose, onPost }) {
  const [image, setImage] = useState(0);
  return (
    <Drawer title={`Source ${source.id}`} onClose={onClose}>
      <div className="drawer-content">
        <Media name={source.media[image]} />
        {source.media.length > 1 && (
          <div className="row">
            <button
              className="btn"
              onClick={() =>
                setImage(
                  (image + source.media.length - 1) % source.media.length,
                )
              }
            >
              Previous media
            </button>
            <span className="num">
              {image + 1}/{source.media.length}
            </span>
            <button
              className="btn"
              onClick={() => setImage((image + 1) % source.media.length)}
            >
              Next media
            </button>
          </div>
        )}
        <div className="row">
          <span className="chip tone">{source.platform}</span>
          <span className="chip tone b-acc">{source.brand}</span>
          <span className="muted">{source.age}</span>
        </div>
        <p className="source-copy">{source.text}</p>
        <p className="muted">
          Immutable fixture source · {source.media.length} media items · Source
          URL and live engagement statistics unavailable.
        </p>
        <h3>Derived drafts</h3>
        {posts
          .filter((p) => p.sourceId === source.id)
          .map((p) => (
            <button
              className="btn btn-full"
              key={p.id}
              onClick={() => onPost(p.id)}
            >
              {p.id} · r{p.revision} · {lanes[p.status]}
            </button>
          ))}
      </div>
    </Drawer>
  );
}
function Editor({ post, source, run, act, onClose, readonly }) {
  const [caption, setCaption] = useState(post.caption);
  const [time, setTime] = useState(post.scheduleAt);
  const [destinations, setDestinations] = useState(post.destinations);
  const [instruction, setInstruction] = useState("");
  const [message, setMessage] = useState("");
  useEffect(() => {
    setCaption(post.caption);
    setTime(post.scheduleAt);
    setDestinations(post.destinations);
  }, [post.revision]);
  function change(fn, text) {
    if (act(fn)) setMessage(text);
  }
  const dirty = caption !== post.caption;
  const planDirty =
    time !== post.scheduleAt ||
    JSON.stringify(destinations) !== JSON.stringify(post.destinations);
  return (
    <Drawer title={`Post ${post.id}`} wide onClose={onClose}>
      <div className="editor-summary">
        <Badge post={post} />
        <span className="num">
          r{post.revision} · from {post.sourceId}
          {run ? ` · ${run.id}` : ""}
        </span>
        {post.needsYou && <span className="badge b-fail">{post.needsYou}</span>}
      </div>
      {(dirty || planDirty) && (
        <p className="notice editor-unsaved" role="status">
          Unsaved caption / planning edits. Save or discard them before another
          material action.
        </p>
      )}
      <div className="editor-grid">
        <section className="editor-fields">
          <div className="card pad">
            <div className="row">
              <label htmlFor="caption" className="slabel">
                Caption · r{post.revision}
              </label>
              {post.lease && (
                <>
                  <span className="badge b-warn">
                    Simulated writer is drafting…
                  </span>
                  <button
                    className="btn btn-sm"
                    disabled={readonly}
                    onClick={() =>
                      change(
                        () => studio.takeOver(post.id),
                        "Simulated lease taken over.",
                      )
                    }
                  >
                    Take over
                  </button>
                </>
              )}
            </div>
            <textarea
              className="field"
              id="caption"
              value={caption}
              maxLength={2200}
              disabled={readonly || post.lease}
              onChange={(e) => setCaption(e.target.value)}
            />
            <p className="muted">
              {caption.length}/2200 characters · Protected terms are not
              live-validated.
            </p>
            <div className="row">
              <button
                className="btn btn-primary"
                disabled={readonly || post.lease || !dirty || planDirty}
                onClick={() =>
                  change(
                    () => studio.edit(post.id, caption, post.revision),
                    "New revision by you. Local review and approval invalidated.",
                  )
                }
              >
                Save revision
              </button>
              <button
                className="btn"
                disabled={!dirty}
                onClick={() => setCaption(post.caption)}
              >
                Discard
              </button>
            </div>
          </div>
          <div className="card pad">
            <h3>Image · fixture media</h3>
            <Media name={post.media[0]} />
            <div className="row">
              <button
                className="btn"
                disabled
                title="Uploads need the production asset SDK"
              >
                Upload unavailable
              </button>
              <button
                className="btn"
                disabled={readonly || dirty || planDirty}
                onClick={() =>
                  change(
                    () =>
                      studio.material(
                        post.id,
                        { media: ["local sample image"] },
                        post.revision,
                      ),
                    "Fixture image revised. Review again.",
                  )
                }
              >
                Use fixture image
              </button>
            </div>
          </div>
          <div className="card pad">
            <label htmlFor="instruction" className="slabel">
              Ask the agent · simulated
            </label>
            <div className="row">
              <input
                className="field"
                id="instruction"
                value={instruction}
                onChange={(e) => setInstruction(e.target.value)}
                placeholder="Make it shorter, more playful…"
              />
              <button
                className="btn"
                disabled={
                  readonly ||
                  !instruction.trim() ||
                  post.lease ||
                  dirty ||
                  planDirty
                }
                onClick={() =>
                  change(
                    () => studio.ask(post.id, instruction, post.revision),
                    "Fixture writer revision appended your instruction; no AI ran.",
                  )
                }
              >
                Simulate ask
              </button>
            </div>
            <p className="muted">
              Appends your instruction unchanged; inspect history for the diff.
            </p>
            <button
              className="btn btn-sm"
              disabled={
                readonly || post.history.length < 2 || dirty || planDirty
              }
              onClick={() =>
                change(
                  () => studio.undo(post.id, post.revision),
                  "Undo recorded as a new revision; approval remains invalid.",
                )
              }
            >
              Undo caption as revision
            </button>
          </div>
          <div className="card pad">
            <h3>Time & destinations</h3>
            <label htmlFor="schedule-at">
              Local planning time · {fixtureTimeZone}
            </label>
            <input
              id="schedule-at"
              className="field"
              type="datetime-local"
              value={time}
              onChange={(e) => setTime(e.target.value)}
              disabled={readonly}
            />
            <div className="row destinations">
              {["instagram", "facebook", "web"].map((destination) => (
                <label key={destination}>
                  <input
                    type="checkbox"
                    checked={destinations.includes(destination)}
                    disabled={readonly}
                    onChange={(e) =>
                      setDestinations(
                        e.target.checked
                          ? [...destinations, destination]
                          : destinations.filter((d) => d !== destination),
                      )
                    }
                  />{" "}
                  {destination}
                </label>
              ))}
            </div>
            <button
              className="btn"
              disabled={readonly || !planDirty || dirty}
              onClick={() =>
                change(
                  () =>
                    studio.material(
                      post.id,
                      { scheduleAt: time, destinations },
                      post.revision,
                    ),
                  "Local planning revised. Material review and approval invalidated.",
                )
              }
            >
              Save local plan
            </button>
            <button
              className="btn"
              disabled={!planDirty}
              onClick={() => {
                setTime(post.scheduleAt);
                setDestinations(post.destinations);
              }}
            >
              Discard local plan
            </button>
            {(dirty || planDirty) && (
              <p className="muted">
                Save or discard the current caption / planning edits before
                another material action.
              </p>
            )}
            <p className="muted">
              Planning only. No external scheduler or delivery is created.
            </p>
          </div>
          <div className="card pad">
            <h3>Review & local staging</h3>
            <div className="row">
              <button
                className="btn"
                disabled={
                  readonly ||
                  dirty ||
                  planDirty ||
                  post.lease ||
                  post.reviewedRevision === post.revision
                }
                onClick={() =>
                  change(
                    () => studio.review(post.id, post.revision),
                    "Current material revision reviewed locally.",
                  )
                }
              >
                Mark reviewed
              </button>
              <button
                className="btn"
                disabled={
                  readonly ||
                  dirty ||
                  planDirty ||
                  post.reviewedRevision !== post.revision ||
                  !post.scheduleAt ||
                  post.approvalRevision === post.revision
                }
                onClick={() =>
                  change(
                    () => studio.approvePlan(post.id, post.revision),
                    "Local plan approval pinned to this material revision.",
                  )
                }
              >
                Approve local plan
              </button>
              <button
                className="btn btn-primary"
                disabled={
                  readonly ||
                  dirty ||
                  planDirty ||
                  post.approvalRevision !== post.revision ||
                  !!post.outbox
                }
                onClick={() =>
                  change(
                    () => studio.stage(post.id, post.revision),
                    "Simulation recorded · No external receipt · Nothing published.",
                  )
                }
              >
                Stage locally
              </button>
            </div>
            {post.outbox && (
              <p className="notice" role="status">
                Simulation recorded · No external receipt · Nothing published.
              </p>
            )}
            <p className="muted">
              Caption, media, date/time or destination edits void this
              revision’s local review and approval.
            </p>
          </div>
          {message && (
            <p className="notice" role="status">
              {message}
            </p>
          )}
        </section>
        <section className="social-preview">
          <p className="slabel">Preview · fixture</p>
          <div className="card">
            <header className="preview-head">
              <strong>{post.brand || "No brand context"}</strong>
              <span className="muted">Local preview</span>
            </header>
            <Media name={post.media[0]} />
            <div className="preview-caption">
              <p>{caption}</p>
              <p className="num muted">
                {post.scheduleAt || "Unplanned"} ·{" "}
                {post.destinations.join(" + ")}
              </p>
            </div>
          </div>
          <p className="muted">
            External receipts: none. Seed published states are illustrative
            only.
          </p>
          <details>
            <summary>Original source</summary>
            <p>{source?.text}</p>
          </details>
        </section>
        <section className="history">
          <p className="slabel">History</p>
          <div className="card pad">
            {post.history.map((entry) => (
              <article key={entry.revision}>
                <div className="row">
                  <strong className="num">r{entry.revision}</strong>
                  <span className="chip tone">{entry.by}</span>
                </div>
                <p>{entry.caption}</p>
                <small className="muted">{entry.note}</small>
              </article>
            ))}
          </div>
        </section>
      </div>
    </Drawer>
  );
}
export default function SocialContent() {
  const [snapshot, setSnapshot] = useState(studio.read);
  const [view, setView] = useState("Home");
  const [context, setContext] = useState("all");
  const [scenario, setScenario] = useState("normal");
  const [mode, setMode] = useState("Calendar");
  const scheduleRef = useRef(null);
  const scrollPositions = useRef({ Calendar: 0, Board: 0 });
  useEffect(() => {
    if (scheduleRef.current)
      scheduleRef.current.scrollTop = scrollPositions.current[mode];
  }, [view, mode]);
  const [week, setWeek] = useState(0);
  const [calendarEdges, setCalendarEdges] = useState({
    earlier: false,
    later: false,
  });
  useEffect(() => {
    const element = scheduleRef.current;
    if (!element || mode !== "Calendar") return;
    function update() {
      const overflow = element.scrollHeight > element.clientHeight + 1;
      setCalendarEdges({
        earlier: overflow && element.scrollTop > 1,
        later:
          overflow &&
          element.scrollTop + element.clientHeight < element.scrollHeight - 1,
      });
    }
    update();
    element.addEventListener("scroll", update, { passive: true });
    window.addEventListener("resize", update);
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(update);
    observer?.observe(element);
    return () => {
      element.removeEventListener("scroll", update);
      window.removeEventListener("resize", update);
      observer?.disconnect();
    };
  }, [view, mode, week, context, snapshot, scenario]);
  function scrollCalendar(direction) {
    const element = scheduleRef.current;
    const day = element.querySelector(".calendar-day");
    element.scrollBy({
      top: direction * ((day?.getBoundingClientRect().height || 84) + 6),
      behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches
        ? "auto"
        : "smooth",
    });
  }
  const [newOnly, setNewOnly] = useState(true);
  const [selection, setSelection] = useState([]);
  const [drawer, setDrawer] = useState(null);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [theme, setTheme] = useState(() => {
    try {
      const saved = window.localStorage.getItem("cadence-theme");
      return saved === "light" || saved === "dark" ? saved : "system";
    } catch {
      return "system";
    }
  });
  const readonly = scenario === "readonly";
  function act(fn) {
    try {
      fn();
      setSnapshot(studio.read());
      setError("");
      return true;
    } catch (e) {
      setError(e.message);
      return false;
    }
  }
  function cycleTheme() {
    const next = { system: "light", light: "dark", dark: "system" }[theme];
    try {
      if (next === "system") window.localStorage.removeItem("cadence-theme");
      else window.localStorage.setItem("cadence-theme", next);
    } catch {
      /* page-only preference */
    }
    if (next === "system")
      document.documentElement.removeAttribute("data-theme");
    else document.documentElement.setAttribute("data-theme", next);
    setTheme(next);
  }
  function openPost(id) {
    setDrawer({ kind: "post", id });
  }
  const sources = snapshot.sources.filter(
    (source) => context === "all" || source.brand === context,
  );
  const posts = snapshot.posts.filter(
    (post) => context === "all" || post.brand === context,
  );
  const visibleRuns = snapshot.runs
    .map((run) => ({
      ...run,
      totalItems: run.items.length,
      items: run.items.filter((item) =>
        posts.some((post) => post.id === item.postId),
      ),
    }))
    .filter((run) => run.items.length);
  const needs = posts.filter(
    (post) => post.needsYou || post.status === "waiting",
  );
  const start = new Date(`${fixtureWeek}T12:00:00Z`);
  start.setUTCDate(start.getUTCDate() + week * 7);
  const days = Array.from({ length: 7 }, (_, index) => {
    const day = new Date(start);
    day.setUTCDate(start.getUTCDate() + index);
    return day;
  });
  const dateKey = (date) => date.toISOString().slice(0, 10);
  function card(post) {
    return (
      <button
        className={`post-card card status-${post.status}`}
        key={post.id}
        onClick={() => openPost(post.id)}
        onFocus={(event) =>
          event.currentTarget.scrollIntoView?.({
            block: "nearest",
            inline: "nearest",
          })
        }
      >
        <small className="num muted">
          {post.scheduleAt.slice(11) || "Unplanned"} · {post.id}
        </small>
        <span>{post.caption}</span>
        <span className="post-footer">
          <Badge post={post} />
          <small className="muted destinations-copy">
            {post.destinations
              .map(
                (destination) =>
                  ({ instagram: "IG", facebook: "FB", web: "Web" })[
                    destination
                  ],
              )
              .join(" · ")}
          </small>
        </span>
      </button>
    );
  }
  const currentPost =
    drawer?.kind === "post"
      ? snapshot.posts.find((post) => post.id === drawer.id)
      : null;
  return (
    <div className="shell app-shell">
      <aside className="side app-side">
        <a className="brand" href="/">
          <Glyph icon={CubeIcon} /> cadence <small>apps</small>
        </a>
        <div className="nav">
          <span className="navlink" aria-current="page">
            <Glyph icon={CubeIcon} /> Apps
          </span>
        </div>
        <div className="side-label slabel">Social Content</div>
        <p className="muted side-description">
          Private app development.
          <br />
          No production connection.
        </p>
        <div className="side-foot">
          <span className="slabel">Development · Fixtures only</span>
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
            onClick={() => {
              act(() => studio.reset());
              setSelection([]);
              setDrawer(null);
              setScenario("normal");
            }}
          >
            Reset fixtures
          </button>
        </div>
      </aside>
      <div className="main">
        <header className="topbar">
          <div className="crumb">
            cadence / apps / <b>social-content</b>
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
        <div className="dev-banner">
          <span>
            Development · Fixtures only · No agents, credentials or external
            delivery.
          </span>
          <small className="num">
            Source base {__APP_DEV_REVISION__} · Live working tree · {started}
          </small>
        </div>
        <div className="body no-rail">
          <main className="content app-content" data-view={view}>
            <nav className="tabs app-tabs" aria-label="Content studio sections">
              {sections.map((section) => (
                <button
                  key={section}
                  className={`tab ${view === section ? "on" : ""}`}
                  aria-current={view === section ? "page" : undefined}
                  onClick={() => {
                    setView(section);
                    setNotice("");
                  }}
                >
                  {section}
                  {section === "Needs you" && (
                    <span className="chip tone b-fail">{needs.length}</span>
                  )}
                </button>
              ))}
            </nav>
            <div className="context-row">
              <label htmlFor="context" className="sr-only">
                Optional brand context
              </label>
              <select
                id="context"
                className="field context-select"
                value={context}
                onChange={(e) => {
                  setContext(e.target.value);
                  setSelection([]);
                  setDrawer(null);
                  setNotice("");
                }}
              >
                <option value="all">All brand contexts</option>
                <option>Kura Ramen</option>
                <option>Velvet Padel</option>
              </select>
              <span className="kicker">content studio · fixture planning</span>
              <button
                className="btn btn-sm"
                disabled
                title="Blog workflow integration is unavailable in this fixture preview"
              >
                Write a blog post
              </button>
            </div>
            {error && (
              <p className="error" role="alert">
                {error}
              </p>
            )}
            {notice && (
              <p className="notice" role="status">
                {notice}
              </p>
            )}
            {readonly && (
              <p className="notice">
                Read-only preview: local edits are disabled.
              </p>
            )}
            {scenario === "loading" ? (
              <div
                className="skeleton-grid"
                role="status"
                aria-label="Loading simulated records"
              >
                {[1, 2, 3].map((n) => (
                  <div className="skeleton card" key={n} />
                ))}
              </div>
            ) : scenario === "error" ? (
              <div className="error" role="alert">
                Simulated fixture read error.{" "}
                <button className="btn" onClick={() => setScenario("normal")}>
                  Retry fixtures
                </button>
              </div>
            ) : scenario === "empty" ? (
              <div className="empty">
                <h2>No fixture records in this scenario</h2>
                <button className="btn" onClick={() => setScenario("normal")}>
                  Return to fixtures
                </button>
              </div>
            ) : (
              <>
                {view === "Home" && (
                  <>
                    <div className="section-toolbar">
                      <h2>Schedule</h2>
                      <div className="tabs mode-tabs">
                        {["Calendar", "Board"].map((value) => (
                          <button
                            key={value}
                            className={`tab ${mode === value ? "on" : ""}`}
                            aria-pressed={mode === value}
                            onClick={() => setMode(value)}
                          >
                            {value}
                          </button>
                        ))}
                      </div>
                      <span className="spacer" />
                      <div className="week-nav">
                        <button
                          className="btn btn-sm"
                          onClick={() => setWeek(week - 1)}
                          aria-label="Previous week"
                        >
                          ←
                        </button>
                        <button
                          className="btn btn-sm"
                          onClick={() => setWeek(0)}
                        >
                          This week
                        </button>
                        <button
                          className="btn btn-sm"
                          onClick={() => setWeek(week + 1)}
                          aria-label="Next week"
                        >
                          →
                        </button>
                      </div>
                    </div>
                    <p className="kicker">
                      {dateKey(days[0])} — {dateKey(days[6])} ·{" "}
                      {fixtureTimeZone} · local plans only
                    </p>
                    <div className="schedule-layout">
                      <div className={`schedule-pane ${mode.toLowerCase()}`}>
                        <div
                          className={`schedule-view ${mode.toLowerCase()}`}
                          role="region"
                          tabIndex={0}
                          aria-label={
                            mode === "Calendar"
                              ? "Week calendar days"
                              : "Post pipeline board"
                          }
                          ref={scheduleRef}
                          onScroll={(event) => {
                            scrollPositions.current[mode] =
                              event.currentTarget.scrollTop;
                          }}
                          key={mode}
                        >
                          {mode === "Calendar" ? (
                            <div className="calendar-agenda">
                              {days.map((day) => (
                                <section
                                  className="calendar-day"
                                  key={dateKey(day)}
                                  data-date={dateKey(day)}
                                >
                                  <header className="agenda-head">
                                    <span className="slabel">
                                      {day.toLocaleDateString(undefined, {
                                        weekday: "short",
                                        timeZone: "UTC",
                                      })}
                                    </span>
                                    <strong className="num">
                                      {day.getUTCDate()}
                                    </strong>
                                    <span className="chip tone num">
                                      {postsForDay(posts, dateKey(day)).length}{" "}
                                      {postsForDay(posts, dateKey(day))
                                        .length === 1
                                        ? "post"
                                        : "posts"}
                                    </span>
                                  </header>
                                  <DayPosts date={dateKey(day)}>
                                    {postsForDay(posts, dateKey(day)).map(card)}
                                    {postsForDay(posts, dateKey(day)).length ===
                                      0 && (
                                      <p className="agenda-empty muted">
                                        No posts planned
                                      </p>
                                    )}
                                  </DayPosts>
                                </section>
                              ))}
                            </div>
                          ) : (
                            <>
                              {Object.entries(lanes).map(([state, label]) => (
                                <BoardLane
                                  key={state}
                                  label={label}
                                  posts={posts.filter(
                                    (post) =>
                                      post.status === state && !post.needsYou,
                                  )}
                                  renderCard={card}
                                />
                              ))}
                              {posts.some((post) => post.needsYou) && (
                                <BoardLane
                                  label="Needs you"
                                  posts={posts.filter((post) => post.needsYou)}
                                  renderCard={card}
                                />
                              )}
                            </>
                          )}
                        </div>
                        {mode === "Calendar" && (
                          <>
                            <button
                              type="button"
                              className="btn ghost calendar-scroll-arrow earlier"
                              hidden={!calendarEdges.earlier}
                              disabled={!calendarEdges.earlier}
                              aria-label="Scroll earlier calendar days"
                              onClick={() => scrollCalendar(-1)}
                            >
                              ↑
                            </button>
                            <button
                              type="button"
                              className="btn ghost calendar-scroll-arrow later"
                              hidden={!calendarEdges.later}
                              disabled={!calendarEdges.later}
                              aria-label="Scroll later calendar days"
                              onClick={() => scrollCalendar(1)}
                            >
                              ↓
                            </button>
                          </>
                        )}
                      </div>
                      <aside className="suggestions-rail">
                        <section className="card pad">
                          <h3>Needs you · {needs.length}</h3>
                          {needs.map((post) => (
                            <button
                              className="lnk needs-link"
                              key={post.id}
                              onClick={() => openPost(post.id)}
                            >
                              {post.id} ·{" "}
                              {post.needsYou ||
                                "Local schedule digest awaiting approval"}
                            </button>
                          ))}
                        </section>
                        <h3 className="slabel">
                          Suggestions · {snapshot.suggestions.length}
                        </h3>
                        {snapshot.suggestions
                          .filter((s) => posts.some((p) => p.id === s.postId))
                          .map((suggestion) => (
                            <section
                              className="card pad suggestion"
                              key={suggestion.id}
                            >
                              <span className="chip tone">
                                editor · fixture
                              </span>
                              <p>{suggestion.text}</p>
                              <small className="muted">{suggestion.note}</small>
                              <div className="row">
                                <button
                                  className="btn btn-primary"
                                  disabled={readonly}
                                  onClick={() => {
                                    if (
                                      act(() =>
                                        studio.suggestion(suggestion.id, true),
                                      )
                                    )
                                      setNotice(
                                        "Fixture suggestion accepted as a new revision by you.",
                                      );
                                  }}
                                >
                                  Accept
                                </button>
                                <button
                                  className="btn"
                                  disabled={readonly}
                                  onClick={() => {
                                    if (
                                      act(() =>
                                        studio.suggestion(suggestion.id, false),
                                      )
                                    )
                                      setNotice("Fixture suggestion rejected.");
                                  }}
                                >
                                  Reject
                                </button>
                                <button
                                  className="lnk"
                                  onClick={() => openPost(suggestion.postId)}
                                >
                                  Open post
                                </button>
                              </div>
                            </section>
                          ))}
                      </aside>
                    </div>
                  </>
                )}
                {view === "Library" && (
                  <>
                    <h2>
                      Library <span className="kicker">source posts</span>
                    </h2>
                    <div className="section-toolbar">
                      <button
                        className={`btn ${newOnly ? "btn-primary" : ""}`}
                        aria-pressed={newOnly}
                        onClick={() => setNewOnly(!newOnly)}
                      >
                        New only{newOnly ? " ✓" : ""}
                      </button>
                      <span className="muted">
                        {selection.length
                          ? `${selection.length} selected`
                          : `${sources.filter((s) => !newOnly || s.isNew).length} of ${sources.length} shown`}
                      </span>
                      <span className="spacer" />
                      {selection.length > 0 && (
                        <button
                          className="btn"
                          onClick={() => setSelection([])}
                        >
                          Clear selection
                        </button>
                      )}
                      <button
                        className="btn btn-primary"
                        disabled={readonly || !selection.length}
                        onClick={() => {
                          let run;
                          if (
                            act(() => {
                              if (
                                !selection.every((id) =>
                                  sources.some((source) => source.id === id),
                                )
                              )
                                throw new Error(
                                  "Selection changed context. Select visible sources again.",
                                );
                              run = studio.batch(selection);
                            })
                          ) {
                            setSelection([]);
                            setView("Runs");
                            setNotice(
                              `${run.id}: ${run.items.length} fixture drafts copied from immutable sources. No agent ran.`,
                            );
                          }
                        }}
                      >
                        {selection.length
                          ? `Draft ${selection.length} posts`
                          : "Draft posts"}
                      </button>
                    </div>
                    <div className="source-grid">
                      {sources
                        .filter((source) => !newOnly || source.isNew)
                        .map((source) => (
                          <article className="card source-card" key={source.id}>
                            <label className="source-select">
                              <input
                                type="checkbox"
                                aria-label={`Select source ${source.id}`}
                                checked={selection.includes(source.id)}
                                disabled={readonly}
                                onChange={(e) =>
                                  setSelection(
                                    e.target.checked
                                      ? [...selection, source.id]
                                      : selection.filter(
                                          (id) => id !== source.id,
                                        ),
                                  )
                                }
                              />
                            </label>
                            <button
                              className="source-inspect"
                              aria-label={`Inspect source ${source.id}`}
                              onClick={() =>
                                setDrawer({ kind: "source", id: source.id })
                              }
                            >
                              <Media name={source.media[0]} />
                              <div className="source-card-copy">
                                <p>{source.text}</p>
                                <div className="row">
                                  <span className="chip tone">
                                    {source.platform}
                                  </span>
                                  {source.isNew && (
                                    <span className="chip tone b-acc">new</span>
                                  )}
                                  <span className="num muted age">
                                    {source.age}
                                  </span>
                                </div>
                              </div>
                            </button>
                          </article>
                        ))}
                    </div>
                  </>
                )}
                {view === "Runs" && (
                  <>
                    <h2>
                      Runs <span className="kicker">local fixture batches</span>
                    </h2>
                    {visibleRuns.length === 0 ? (
                      <div className="empty">
                        <p>
                          Select Library source cards to start a fixture batch.
                        </p>
                        <button
                          className="btn"
                          onClick={() => setView("Library")}
                        >
                          Open Library
                        </button>
                      </div>
                    ) : (
                      visibleRuns.map((run) => (
                        <section className="card pad run-card" key={run.id}>
                          <div className="row">
                            <h3>
                              {run.id} · {run.label}
                            </h3>
                            <span className="badge b-info">{run.status}</span>
                          </div>
                          <p className="muted">
                            {run.items.length} of {run.totalItems} items in this
                            context · copied ·{" "}
                            {
                              run.items.filter(
                                (item) =>
                                  snapshot.posts.find(
                                    (p) => p.id === item.postId,
                                  )?.reviewedRevision,
                              ).length
                            }{" "}
                            reviewed · no agent execution
                          </p>
                          <progress
                            max={run.items.length}
                            value={run.items.length}
                            aria-label={`${run.id} copied sources`}
                          />
                          <table className="tbl">
                            <thead>
                              <tr>
                                <th>Source → draft</th>
                                <th>Copy</th>
                                <th>Review</th>
                                <th>Local plan</th>
                              </tr>
                            </thead>
                            <tbody>
                              {run.items.map((item) => {
                                const value = snapshot.posts.find(
                                  (p) => p.id === item.postId,
                                );
                                return (
                                  <tr key={item.postId}>
                                    <td>
                                      <button
                                        className="lnk"
                                        onClick={() => openPost(item.postId)}
                                      >
                                        {item.sourceId} → {item.postId}
                                      </button>
                                    </td>
                                    <td>Copied</td>
                                    <td>
                                      {value.reviewedRevision === value.revision
                                        ? "Reviewed"
                                        : "Needs review"}
                                    </td>
                                    <td>
                                      {value.approvalRevision === value.revision
                                        ? "Approved locally"
                                        : "Unapproved"}
                                    </td>
                                  </tr>
                                );
                              })}
                            </tbody>
                          </table>
                          <details>
                            <summary>Fixture run log</summary>
                            {run.log.map((line, index) => (
                              <p key={index}>{line}</p>
                            ))}
                          </details>
                        </section>
                      ))
                    )}
                  </>
                )}
                {view === "Needs you" && (
                  <>
                    <h2>
                      Needs you{" "}
                      <span className="kicker">
                        local digest & illustrative verification
                      </span>
                    </h2>
                    {needs.length === 0 && (
                      <div className="empty">No local decisions waiting.</div>
                    )}
                    {needs.map((post) => (
                      <section className="card pad digest-card" key={post.id}>
                        <div className="row">
                          <h3>
                            {post.id} · r{post.revision}
                          </h3>
                          <Badge post={post} />
                        </div>
                        <p>{post.caption}</p>
                        <p className="num muted">
                          {post.scheduleAt || "Unplanned"} ·{" "}
                          {post.destinations.join(" + ")}
                        </p>
                        {post.needsYou && (
                          <p className="error">{post.needsYou}</p>
                        )}
                        <div className="row">
                          <button
                            className="btn"
                            onClick={() => openPost(post.id)}
                          >
                            Open post
                          </button>
                          <button
                            className="btn"
                            disabled={readonly}
                            onClick={() => {
                              if (act(() => studio.hold(post.id)))
                                setNotice(
                                  "Local item held back; no external action.",
                                );
                            }}
                          >
                            Hold back
                          </button>
                          <button
                            className="btn btn-primary"
                            disabled={
                              readonly ||
                              post.reviewedRevision !== post.revision ||
                              !post.scheduleAt
                            }
                            onClick={() => {
                              if (
                                act(() =>
                                  studio.approvePlan(post.id, post.revision),
                                )
                              )
                                setNotice(
                                  "Fixture plan approved at this material revision. Nothing sent.",
                                );
                            }}
                          >
                            Approve local plan
                          </button>
                        </div>
                      </section>
                    ))}
                  </>
                )}
                {["Automations", "Workflows", "Settings"].includes(view) && (
                  <SecondaryViews view={view.toLowerCase()} context={context} />
                )}
                {view === "Publish" && <PublishPreview readonly={readonly} />}
              </>
            )}
          </main>
        </div>
      </div>
      {drawer?.kind === "source" && (
        <SourceDrawer
          source={snapshot.sources.find((source) => source.id === drawer.id)}
          posts={snapshot.posts}
          onClose={() => setDrawer(null)}
          onPost={openPost}
        />
      )}
      {currentPost && (
        <Editor
          key={currentPost.id}
          post={currentPost}
          source={snapshot.sources.find(
            (source) => source.id === currentPost.sourceId,
          )}
          run={snapshot.runs.find((run) => run.id === currentPost.runId)}
          act={act}
          readonly={readonly}
          onClose={() => setDrawer(null)}
        />
      )}
    </div>
  );
}
