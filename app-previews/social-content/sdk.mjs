/** Local product fixture facade. Sources are immutable; no provider, receipt or send exists. */
export function createStudioFixture(seed) {
  let state = structuredClone(seed);
  let sequence = Math.max(
    0,
    ...state.posts.map((p) => Number(p.id.split("-").at(-1)) || 0),
  );
  const post = (id) => {
    const value = state.posts.find((p) => p.id === id);
    if (!value) throw new Error("Draft not found.");
    return value;
  };
  function current(value, expected) {
    if (expected !== value.revision)
      throw new Error(
        "Draft changed while you were editing. Reopen and compare the current revision.",
      );
  }
  function revise(value, patch, expected, by = "you", note = "Material edit") {
    current(value, expected);
    if (value.lease) throw new Error("Take over the simulated lease first.");
    const hadApproval = value.approvalRevision !== null;
    Object.assign(value, structuredClone(patch));
    value.revision++;
    value.reviewedRevision = null;
    value.approvalRevision = null;
    value.outbox = null;
    value.status = "review";
    value.needsYou = hadApproval
      ? `Local approval voided by revision ${value.revision}.`
      : null;
    value.history.unshift({
      revision: value.revision,
      by,
      caption: value.caption,
      note,
      scheduleAt: value.scheduleAt,
      destinations: [...value.destinations],
      media: [...value.media],
    });
  }
  return {
    read: () => structuredClone(state),
    batch(sourceIds, brand = "") {
      const selected = [...new Set(sourceIds)].map((id) =>
        state.sources.find((s) => s.id === id),
      );
      if (!selected.length || selected.some((s) => !s))
        throw new Error("Select valid source posts.");
      let runId = state.runs.length + 1;
      while (state.runs.some((run) => run.id === `run-${runId}`)) runId++;
      const run = {
        id: `run-${runId}`,
        label: `Draft ${selected.length} posts`,
        status: "fixture complete",
        items: [],
        log: [
          "Local fixture batch created. Source text copied unchanged; no agent ran.",
        ],
      };
      for (const source of selected) {
        const value = {
          id: `fixture-${++sequence}`,
          sourceId: source.id,
          brand: brand || source.brand,
          caption: source.text,
          media: [...source.media],
          revision: 1,
          status: "draft",
          scheduleAt: "",
          destinations: ["instagram"],
          reviewedRevision: null,
          approvalRevision: null,
          needsYou: null,
          lease: false,
          history: [
            {
              revision: 1,
              by: "fixture copy",
              caption: source.text,
              note: `From ${source.id}; no generated copy.`,
            },
          ],
          runId: run.id,
          outbox: null,
        };
        state.posts.unshift(value);
        run.items.push({ postId: value.id, sourceId: source.id });
      }
      state.runs.unshift(run);
      return structuredClone(run);
    },
    edit(id, caption, expected) {
      if (!caption.trim()) throw new Error("Caption cannot be empty.");
      if (caption.length > 2200)
        throw new Error("Keep the caption within 2200 characters.");
      const value = post(id);
      if (value.lease)
        throw new Error("Take over the simulated writer lease before editing.");
      revise(value, { caption }, expected);
    },
    takeOver(id) {
      post(id).lease = false;
    },
    material(id, patch, expected) {
      const value = post(id);
      if (
        !Object.keys(patch).every((key) =>
          ["scheduleAt", "destinations", "media"].includes(key),
        )
      )
        throw new Error("Unsupported material field.");
      if (
        patch.scheduleAt !== undefined &&
        patch.scheduleAt !== "" &&
        !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(patch.scheduleAt)
      )
        throw new Error("Choose a local date and time.");
      if (
        patch.destinations &&
        (!patch.destinations.length ||
          !patch.destinations.every((d) =>
            ["instagram", "facebook", "web"].includes(d),
          ))
      )
        throw new Error("Choose supported destinations.");
      revise(value, patch, expected);
    },
    review(id, expected) {
      const value = post(id);
      current(value, expected);
      if (value.lease) throw new Error("Take over the simulated lease first.");
      value.approvalRevision = null;
      value.outbox = null;
      value.reviewedRevision = value.revision;
      value.status = "waiting";
      value.needsYou = null;
    },
    approvePlan(id, expected) {
      const value = post(id);
      current(value, expected);
      if (value.reviewedRevision !== value.revision)
        throw new Error(
          "Review the current material revision before approving a local plan.",
        );
      if (!value.scheduleAt || !value.destinations.length)
        throw new Error("Choose a local time and destinations first.");
      value.approvalRevision = value.revision;
      value.status = "scheduled";
      value.needsYou = null;
    },
    hold(id) {
      const value = post(id);
      value.approvalRevision = null;
      value.outbox = null;
      value.status = "waiting";
    },
    stage(id, expected) {
      const value = post(id);
      current(value, expected);
      if (
        value.reviewedRevision !== value.revision ||
        value.approvalRevision !== value.revision
      )
        throw new Error(
          "Review and approve the current local plan before staging.",
        );
      if (value.outbox)
        throw new Error("This fixture revision is already staged.");
      value.outbox = {
        kind: "simulation",
        revision: value.revision,
        externalReceipt: null,
      };
      return structuredClone(value.outbox);
    },
    ask(id, instruction, expected) {
      if (!instruction.trim())
        throw new Error("Add an instruction for the fixture revision.");
      const value = post(id);
      if (value.lease) throw new Error("Take over the simulated lease first.");
      const caption = `${value.caption}\n[Fixture instruction: ${instruction.trim()}]`;
      if (caption.length > 2200)
        throw new Error("Fixture revision would exceed 2200 characters.");
      revise(
        value,
        { caption },
        expected,
        "writer (fixture, asked by you)",
        "Instruction appended unchanged; no AI generated copy.",
      );
    },
    undo(id, expected) {
      const value = post(id);
      const previous = value.history[1];
      if (!previous) throw new Error("No earlier revision to restore.");
      revise(
        value,
        {
          caption: previous.caption,
        },
        expected,
        "you",
        "Undo as a new revision; old approval is not restored.",
      );
    },
    suggestion(id, accept) {
      const suggestion = state.suggestions.find((s) => s.id === id);
      if (!suggestion) throw new Error("Suggestion not found.");
      if (accept) {
        const value = post(suggestion.postId);
        revise(
          value,
          { caption: suggestion.text },
          value.revision,
          "you",
          "Accepted fixture suggestion.",
        );
      }
      state.suggestions = state.suggestions.filter((s) => s.id !== id);
    },
    reset() {
      state = structuredClone(seed);
      sequence = Math.max(
        0,
        ...state.posts.map((p) => Number(p.id.split("-").at(-1)) || 0),
      );
    },
  };
}
