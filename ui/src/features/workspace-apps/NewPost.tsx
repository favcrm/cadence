import { useEffect, useState, type FormEvent } from "react";
import Button from "../../ui/Button";
import Select, { type SelectOption } from "../../ui/Select";
import { WorkspaceDialog } from "./WorkspaceDialog";
import { ApiError } from "../../lib/api";
import { workspaceApps, type CapabilityQuote } from "./workspaceApps";
import { effectivePrompt, promptDefault, promptError } from "./promptFields";

export interface NewPostValues {
  title: string;
  source: string;
  writer: string;
  reviewer: string;
  ownerPm: string;
  workflow: string;
  contentPrompt?: string;
  imagePrompt?: string;
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
  workflowInputs = {},
  contextDefaults,
  contextLabel,
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
  workflowInputs?: Record<string, { name: string; default?: string | null }[]>;
  contextDefaults?: Record<string, string>;
  contextLabel?: string;
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
  const [contentPrompt, setContentPrompt] = useState("");
  const [imagePrompt, setImagePrompt] = useState("");
  const [validation, setValidation] = useState<string | null>(null);
  const [imageQuote, setImageQuote] = useState<CapabilityQuote | null>(null);
  const [imageQuoteError, setImageQuoteError] = useState<string | null>(null);
  useEffect(() => {
    setImageQuote(null); setImageQuoteError(null);
    if (!imageSupported || !imageBindingDigest) return;
    const controller = new AbortController();
    void workspaceApps.bindingQuote(installId, "image", contextId, controller.signal).then(value => {
      if (controller.signal.aborted) return;
      if (value.slot !== "image" || value.binding_digest !== imageBindingDigest
        || value.quote.schema !== 1 || value.quote.currency !== "USD"
        || value.quote.units !== 1 || !Number.isSafeInteger(value.quote.unit_price_micros)
        || value.quote.unit_price_micros <= 0
        || value.quote.total_price_micros !== value.quote.unit_price_micros
        || typeof value.quote.price_revision !== "string" || !value.quote.price_revision) {
        setImageQuoteError("The image connection changed or is unavailable. Refresh Settings before planning an image.");
        return;
      }
      setImageQuote(value);
    }).catch(reason => {
      if (controller.signal.aborted) return;
      if (reason instanceof ApiError && [401, 403].includes(reason.status)) { onDenied(); return; }
      setImageQuoteError(reason instanceof Error ? reason.message : "Could not reach the image provider.");
    });
    return () => controller.abort();
  }, [installId, contextId, imageSupported, imageBindingDigest, onDenied]);
  const availableWorkflows = workflows.filter(option =>
    option.value !== "image-instagram" || !!sourceChoice,
  ).filter(option => option.value !== "image-manual" || !sourceChoice).map(option => ({
    ...option,
    disabled: option.disabled || ((option.value === "image-instagram" || option.value === "image-manual") && !imageQuote),
  }));
  const imageWorkflow = workflow === "image-instagram" || workflow === "image-manual";
  const contentDefault = promptDefault("content_prompt", workflowInputs[workflow], contextDefaults);
  const imageDefault = promptDefault("image_prompt", workflowInputs[workflow], contextDefaults);
  const contentEffective = effectivePrompt(contentPrompt, contentDefault);
  const imageEffective = effectivePrompt(imagePrompt, imageDefault);
  const promptValidation = promptError(contentEffective.value, "Content prompt") ||
    (imageWorkflow ? promptError(imageEffective.value, "Image prompt") : null);
  useEffect(() => {
    setContentPrompt(contentDefault.value);
    setImagePrompt(imageDefault.value);
  }, [workflow, contentDefault.value, imageDefault.value]);
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
          ? "Choose a retained Instagram source and wait for the image option to be ready."
        : workflow === "image-manual" && (!imageQuote || !!sourceChoice)
          ? "Use pasted facts and wait for the image option to be ready."
        : promptValidation
          ? promptValidation
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
      ...(contentEffective.origin === "run_override" ? { contentPrompt: contentEffective.value } : {}),
      ...(imageWorkflow && imageEffective.origin === "run_override" ? { imagePrompt: imageEffective.value } : {}),
      ...(sourceChoice ? { sourceReceiptId: sourceChoice.receiptId, selectedPostId: sourceChoice.postId } : {}),
    });
  };
  return (
    <WorkspaceDialog title="New post" onClose={onClose}>
      <form className="wa-stack" onSubmit={submit}>
        <p className="wa-muted">
          {sourceChoice ? "Review the chosen retained Instagram post before making a new caption. An image option appears when a draft provider is bound." : "Paste the facts your team should use. You’ll review the frozen plan"}
          {!sourceChoice && " before the writer starts. Releasing the accepted text is a separate decision."}
        </p>
        {(error || validation) && (
          <p className="wa-alert" data-tone="fail" role="alert">
            {error || validation}
          </p>
        )}
        {imageSupported && (!imageBindingDigest
          ? <p className="wa-alert">To add an image, choose a ready image generation connection in Cadence Settings.</p>
          : imageQuote
            ? <p className="wa-alert">The image option is ready — the frozen plan needs your approval before the writer starts.</p>
            : imageQuoteError
              ? <p className="wa-alert" data-tone="fail" role="alert">{imageQuoteError}</p>
              : <p className="wa-muted" role="status">Checking the image option…</p>)}
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
            {sourceChoice ? <>From @{sourceChoice.handle} · <a href={sourceChoice.permalink} target="_blank" rel="noopener noreferrer">View original post</a> · receipt {sourceChoice.receiptId}. The selected text cannot be edited while linked to this receipt.</> : "Paste one paragraph without line breaks. A ready image connection also allows a separate image draft from these facts."}
          </p>
          {sourceChoice && <Button onClick={() => { setSourceChoice(null); setSource(""); if (workflow === "image-instagram") setWorkflow(""); }}>Use pasted facts instead</Button>}
        </div>
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
          <label htmlFor="wa-content-prompt">Content prompt</label>
          <textarea
            id="wa-content-prompt"
            name="content_prompt"
            className="wa-input wa-prompt-input"
            value={contentPrompt}
            onChange={event => setContentPrompt(event.target.value)}
            disabled={busy || !workflow || !workflowInputs[workflow]?.some(input => input.name === "content_prompt")}
            maxLength={512}
            aria-describedby="wa-content-prompt-help"
          />
          <p id="wa-content-prompt-help" className="wa-kicker">
            {contentEffective.origin === "run_override" ? "This post overrides " : "Using "}
            {contentDefault.origin === "context_default" ? `${contextLabel || "brand"}’s saved default` : "the app default"}.
            Clear the field to use that default. This guides wording; source facts and review rules still apply.
          </p>
        </div>
        <div className="wa-field">
          <label htmlFor="wa-image-prompt">Image prompt</label>
          <textarea
            id="wa-image-prompt"
            name="image_prompt"
            className="wa-input wa-prompt-input"
            value={imagePrompt}
            onChange={event => setImagePrompt(event.target.value)}
            disabled={busy || !imageWorkflow || !imageQuote || !workflowInputs[workflow]?.some(input => input.name === "image_prompt")}
            maxLength={512}
            aria-describedby="wa-image-prompt-help"
          />
          <p id="wa-image-prompt-help" className="wa-kicker">
            {imageWorkflow && imageQuote
              ? <>{imageEffective.origin === "run_override" ? "This post overrides " : "Using "}{imageDefault.origin === "context_default" ? `${contextLabel || "brand"}’s saved default` : "the app default"}. Clear the field to use that default. The provider and asset rules stay fixed.</>
              : "Choose an image workflow with a ready generated image connection to edit this prompt."}
          </p>
        </div>
        {workflow && <section className="wa-panel wa-stack" aria-label="Effective prompts for this plan">
          <h3>Effective prompts for this plan</h3>
          <p><strong>Content · {contentEffective.origin === "run_override" ? "this post’s override" : contentEffective.origin === "context_default" ? "saved brand default" : "app default"}</strong><br />{contentEffective.value || "No prompt default is installed for this workflow."}</p>
          {imageWorkflow && <p><strong>Image · {imageEffective.origin === "run_override" ? "this post’s override" : imageEffective.origin === "context_default" ? "saved brand default" : "app default"}</strong><br />{imageEffective.value || "No prompt default is installed for this workflow."}</p>}
          <p className="wa-kicker">The effective text is frozen when you create the plan. A later default edit cannot change that plan.</p>
        </section>}
        <div className="wa-fields">
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
