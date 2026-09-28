import { useEffect, useState, type FormEvent } from "react";
import Button from "../../ui/Button";
import Select, { type SelectOption } from "../../ui/Select";
import { WorkspaceDialog } from "./WorkspaceDialog";
import { ApiError } from "../../lib/api";
import { workspaceApps, type CapabilityQuote } from "./workspaceApps";

export interface NewPostValues {
  title: string;
  source: string;
  writer: string;
  reviewer: string;
  ownerPm: string;
  workflow: string;
  sourceReceiptId?: string;
  selectedPostId?: string;
}

export interface SelectedSource {
  receiptId: string;
  postId: string;
  handle: string;
  caption: string;
  permalink: string;
}

export function NewPost({
  workflows,
  workers,
  managers,
  busy,
  error,
  onClose,
  onCreate,
  selectedSource,
  installId,
  contextId,
  imageSupported,
  imageBindingDigest,
  onDenied,
}: {
  workflows: SelectOption[];
  workers: SelectOption[];
  managers: SelectOption[];
  busy: boolean;
  error: string | null;
  onClose: () => void;
  onCreate: (values: NewPostValues) => void;
  selectedSource?: SelectedSource | null;
  installId: string;
  contextId?: string;
  imageSupported: boolean;
  imageBindingDigest?: string;
  onDenied: () => void;
}) {
  const [title, setTitle] = useState("");
  const [source, setSource] = useState(selectedSource?.caption ?? "");
  const [sourceChoice, setSourceChoice] = useState(selectedSource ?? null);
  const [writer, setWriter] = useState("");
  const [reviewer, setReviewer] = useState("");
  const [ownerPm, setOwnerPm] = useState("");
  const [workflow, setWorkflow] = useState(
    workflows.length === 1 ? workflows[0].value : "",
  );
  const [validation, setValidation] = useState<string | null>(null);
  const [imageQuote, setImageQuote] = useState<CapabilityQuote | null>(null);
  const [imageQuoteError, setImageQuoteError] = useState<string | null>(null);
  useEffect(() => {
    setImageQuote(null); setImageQuoteError(null);
    if (!sourceChoice || !imageBindingDigest) return;
    const controller = new AbortController();
    void workspaceApps.bindingQuote(installId, "image", contextId, controller.signal).then(value => {
      if (controller.signal.aborted) return;
      if (value.slot !== "image" || value.binding_digest !== imageBindingDigest
        || value.quote.schema !== 1 || value.quote.currency !== "USD"
        || value.quote.units !== 1 || !Number.isSafeInteger(value.quote.unit_price_micros)
        || value.quote.unit_price_micros <= 0
        || value.quote.total_price_micros !== value.quote.unit_price_micros
        || typeof value.quote.price_revision !== "string" || !value.quote.price_revision) {
        setImageQuoteError("The image connection or current provider price changed. Refresh Settings before planning an image.");
        return;
      }
      setImageQuote(value);
    }).catch(reason => {
      if (controller.signal.aborted) return;
      if (reason instanceof ApiError && [401, 403].includes(reason.status)) { onDenied(); return; }
      setImageQuoteError(reason instanceof Error ? reason.message : "Could not obtain the image provider’s price.");
    });
    return () => controller.abort();
  }, [installId, contextId, imageBindingDigest, sourceChoice?.receiptId, onDenied]);
  const availableWorkflows = workflows.filter(option => option.value !== "image-instagram" || !!imageQuote);
  const ownerGroup = managers.some((manager) => manager.value === ownerPm)
    ? ownerPm
    : "";
  const eligibleWorkers = ownerGroup
    ? workers.filter((worker) => worker.group === ownerGroup)
    : [];
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const invalid =
      !title.trim() || !source.trim()
        ? "Enter a post title and source facts."
        : workflow === "image-instagram" && (!sourceChoice || !imageQuote)
          ? "Choose a retained Instagram source and wait for its current image price."
        : /[\r\n\t]/.test(title) || (!sourceChoice && /[\r\n\t]/.test(source))
          ? "Title and source facts must be one paragraph without line breaks or tabs for this workflow."
          : !workflow
            ? "Choose a workflow."
            : !ownerPm
              ? "Choose the responsible PM."
              : !writer || !reviewer
                ? "Choose a writer and an independent reviewer."
                : writer === reviewer
                  ? "The writer and reviewer must be different agents."
                  : !eligibleWorkers.some(
                        (worker) => worker.value === writer,
                      ) ||
                      !eligibleWorkers.some(
                        (worker) => worker.value === reviewer,
                      )
                    ? "Choose registered workers from the responsible PM’s team."
                    : null;
    setValidation(invalid);
    if (invalid || busy) return;
    onCreate({
      title: title.trim(),
      source,
      writer,
      reviewer,
      ownerPm,
      workflow,
      ...(sourceChoice ? { sourceReceiptId: sourceChoice.receiptId, selectedPostId: sourceChoice.postId } : {}),
    });
  };
  return (
    <WorkspaceDialog title="New post" onClose={onClose}>
      <form className="wa-stack" onSubmit={submit}>
        <p className="wa-muted">
          {sourceChoice ? "Review the chosen retained Instagram post before making a new caption. An image option appears when a draft provider is bound; its exact charge will be frozen in the plan." : "Paste the facts your team should use. You’ll review the frozen plan"}
          {!sourceChoice && " before the writer starts. Releasing the accepted text is a separate decision."}
        </p>
        {(error || validation) && (
          <p className="wa-alert" data-tone="fail" role="alert">
            {error || validation}
          </p>
        )}
        {sourceChoice && imageSupported && (!imageBindingDigest
          ? <p className="wa-alert">To add an image, bind an AgenticOS provider.draft connection in Settings.</p>
          : imageQuote
            ? <p className="wa-alert">Current charge for one generated image: <strong>USD {(imageQuote.quote.total_price_micros / 1_000_000).toFixed(6)}</strong>. The frozen plan requires a separate cost approval before dispatch.</p>
            : imageQuoteError
              ? <p className="wa-alert" data-tone="fail" role="alert">{imageQuoteError}</p>
              : <p className="wa-muted" role="status">Checking one-image provider price…</p>)}
        <div className="wa-field">
          <label htmlFor="wa-post-title">Post title</label>
          <input
            id="wa-post-title"
            name="title"
            className="wa-input"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            required
            maxLength={120}
            disabled={busy}
          />
        </div>
        <div className="wa-field">
          <label htmlFor="wa-post-source">Source facts</label>
          <textarea
            id="wa-post-source"
            name="source"
            className="wa-input"
            value={source}
            onChange={(e) => setSource(e.target.value)}
            required
            maxLength={10000}
            disabled={busy}
            readOnly={!!sourceChoice}
            aria-describedby="wa-source-help"
          />
          <p id="wa-source-help" className="wa-kicker">
            {sourceChoice ? <>From @{sourceChoice.handle} · <a href={sourceChoice.permalink} target="_blank" rel="noopener noreferrer">View original post</a> · receipt {sourceChoice.receiptId}. The selected text cannot be edited while linked to this receipt.</> : "Paste one paragraph without line breaks. Select a retained Instagram post to make an image."}
          </p>
          {sourceChoice && <Button onClick={() => { setSourceChoice(null); setSource(""); if (workflow === "image-instagram") setWorkflow(""); }}>Use pasted facts instead</Button>}
        </div>
        <div className="wa-fields">
          <div className="wa-field">
            <label htmlFor="wa-workflow">Workflow</label>
            <Select
              id="wa-workflow"
              value={workflow}
              onChange={setWorkflow}
              options={availableWorkflows}
              placeholder="Choose workflow"
              disabled={busy}
              full
            />
          </div>
          <div className="wa-field">
            <label htmlFor="wa-owner">Responsible PM</label>
            <Select
              id="wa-owner"
              value={ownerPm}
              onChange={(value) => {
                setOwnerPm(value);
                setWriter("");
                setReviewer("");
              }}
              options={managers}
              placeholder="Choose PM"
              disabled={busy}
              full
            />
          </div>
          <div className="wa-field">
            <label htmlFor="wa-writer">Writer</label>
            <Select
              id="wa-writer"
              value={writer}
              onChange={(value) => {
                setWriter(value);
                if (value === reviewer) setReviewer("");
              }}
              options={eligibleWorkers}
              placeholder={ownerPm ? "Choose writer" : "Choose a PM first"}
              disabled={busy || !ownerPm}
              full
            />
          </div>
          <div className="wa-field">
            <label htmlFor="wa-reviewer">Independent reviewer</label>
            <Select
              id="wa-reviewer"
              value={reviewer}
              onChange={setReviewer}
              options={eligibleWorkers.map((option) => ({
                ...option,
                disabled: option.disabled || option.value === writer,
              }))}
              placeholder={ownerPm ? "Choose reviewer" : "Choose a PM first"}
              disabled={busy || !ownerPm}
              full
            />
          </div>
        </div>
        {!workers.length && (
          <p className="wa-alert">
            No registered workers are available. Set up a writer and reviewer
            before creating a post.
          </p>
        )}
        {ownerPm && eligibleWorkers.length < 2 && (
          <p className="wa-alert">
            This PM needs two different registered workers for writing and
            review.
          </p>
        )}
        {!managers.length && (
          <p className="wa-alert">
            No registered PM is available to own this run.
          </p>
        )}
        <Button type="submit" variant="primary" loading={busy}>
          Create plan
        </Button>
      </form>
    </WorkspaceDialog>
  );
}
