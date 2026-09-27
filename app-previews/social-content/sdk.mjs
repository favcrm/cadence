/** App-local development facade. Not CAD-500's privileged production host SDK. */
export function createFixtureSdk(seed = []) {
  let posts = structuredClone(seed),
    sequence = posts.length;
  const get = (id) => {
    const post = posts.find((p) => p.id === id);
    if (!post) throw new Error("Draft not found.");
    return post;
  };
  return {
    read: () => structuredClone(posts),
    create(source, brand = "No brand context") {
      if (!source.trim())
        throw new Error("Add source text before creating a draft.");
      let id;
      do {
        id = `fixture-${++sequence}`;
      } while (posts.some((p) => p.id === id));
      const post = {
        id,
        source,
        caption: source,
        brand,
        revision: 1,
        status: "draft",
        reviewedRevision: null,
      };
      posts = [post, ...posts];
      return structuredClone(post);
    },
    edit(id, caption) {
      if (!caption.trim()) throw new Error("Caption cannot be empty.");
      const p = get(id);
      p.caption = caption;
      p.revision++;
      p.status = "draft";
      p.reviewedRevision = null;
    },
    review(id) {
      const p = get(id);
      p.status = "review";
      p.reviewedRevision = p.revision;
    },
    stage(id) {
      const p = get(id);
      if (p.status !== "review" || p.reviewedRevision !== p.revision)
        throw new Error("Review the current draft revision before staging.");
      p.status = "local-outbox";
      return {
        kind: "simulation",
        revision: p.revision,
        externalReceipt: null,
      };
    },
    reset(seed = []) {
      posts = structuredClone(seed);
      sequence = posts.length;
    },
  };
}
