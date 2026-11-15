import { test } from "node:test";
import assert from "node:assert/strict";
import { Window } from "happy-dom";
import { installSandboxFormActions } from "../src/forms.mjs";

function setupDoc() {
  const win = new Window({ url: "https://frame.local/index.html" });
  return { win, doc: win.document };
}

function awaitableTeardown(win) {
  try {
    win.close();
  } catch {
    // happy-dom cleanup best-effort
  }
}

test("installSandboxFormActions prevents navigation on actionless forms without blocking submit listeners", () => {
  const { win, doc } = setupDoc();
  const cleanup = installSandboxFormActions(doc);

  const form = doc.createElement("form");
  const button = doc.createElement("button");
  button.type = "submit";
  form.append(button);
  doc.body.append(form);

  let bubbleSeen = false;
  let defaultPreventedInBubble = false;
  form.addEventListener("submit", (event) => {
    bubbleSeen = true;
    defaultPreventedInBubble = event.defaultPrevented;
  });

  const evt = new win.Event("submit", { bubbles: true, cancelable: true });
  form.dispatchEvent(evt);

  assert.equal(bubbleSeen, true);
  assert.equal(defaultPreventedInBubble, true);
  assert.equal(evt.defaultPrevented, true);

  cleanup();
  awaitableTeardown(win);
});

test("installSandboxFormActions preserves forms with explicit non-empty action or formaction and cleans up cleanly", () => {
  const { win, doc } = setupDoc();
  const cleanup = installSandboxFormActions(doc);

  const explicitForm = doc.createElement("form");
  explicitForm.setAttribute("action", "https://example.com/submit");
  doc.body.append(explicitForm);

  const explicitEvt = new win.Event("submit", { bubbles: true, cancelable: true });
  explicitForm.dispatchEvent(explicitEvt);
  assert.equal(explicitEvt.defaultPrevented, false);

  const formActionBtn = doc.createElement("button");
  formActionBtn.type = "submit";
  formActionBtn.setAttribute("formaction", "https://example.com/override");
  const emptyActionForm = doc.createElement("form");
  emptyActionForm.setAttribute("action", "   ");
  emptyActionForm.append(formActionBtn);
  doc.body.append(emptyActionForm);

  const formActionSubmit = new win.Event("submit", { bubbles: true, cancelable: true });
  Object.defineProperty(formActionSubmit, "submitter", { value: formActionBtn, configurable: true });
  emptyActionForm.dispatchEvent(formActionSubmit);
  assert.equal(formActionSubmit.defaultPrevented, false);

  const emptyEvt = new win.Event("submit", { bubbles: true, cancelable: true });
  emptyActionForm.dispatchEvent(emptyEvt);
  assert.equal(emptyEvt.defaultPrevented, true);

  cleanup();
  cleanup();

  const afterForm = doc.createElement("form");
  doc.body.append(afterForm);
  const afterEvt = new win.Event("submit", { bubbles: true, cancelable: true });
  afterForm.dispatchEvent(afterEvt);
  assert.equal(afterEvt.defaultPrevented, false);

  awaitableTeardown(win);
});

test("installSandboxFormActions synthesizes submit on actionless submit button clicks and skips non-submit controls", () => {
  const { win, doc } = setupDoc();
  const cleanup = installSandboxFormActions(doc);

  const form = doc.createElement("form");
  const defaultBtn = doc.createElement("button");
  const submitBtn = doc.createElement("button");
  submitBtn.type = "submit";
  const innerSpan = doc.createElement("span");
  innerSpan.textContent = "Save";
  submitBtn.append(innerSpan);

  const typeBtn = doc.createElement("button");
  typeBtn.type = "button";
  const disabledBtn = doc.createElement("button");
  disabledBtn.type = "submit";
  disabledBtn.disabled = true;

  const fieldset = doc.createElement("fieldset");
  fieldset.disabled = true;
  const legend = doc.createElement("legend");
  const legendBtn = doc.createElement("button");
  legendBtn.type = "submit";
  legend.append(legendBtn);
  const fsBtn = doc.createElement("button");
  fsBtn.type = "submit";
  fieldset.append(legend, fsBtn);

  const formActionBtn = doc.createElement("button");
  formActionBtn.type = "submit";
  formActionBtn.setAttribute("formaction", "https://example.com/override");

  form.append(defaultBtn, submitBtn, typeBtn, disabledBtn, fieldset, formActionBtn);

  const explicitForm = doc.createElement("form");
  explicitForm.setAttribute("action", "https://example.com/submit");
  const explicitBtn = doc.createElement("button");
  explicitBtn.type = "submit";
  explicitForm.append(explicitBtn);

  doc.body.append(form, explicitForm);

  const seen = [];
  form.addEventListener("submit", (event) => {
    seen.push({ submitter: event.submitter, defaultPrevented: event.defaultPrevented });
  });

  const dispatchClick = (el) => {
    const evt = new win.MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
    el.dispatchEvent(evt);
    return evt.defaultPrevented;
  };

  assert.equal(dispatchClick(defaultBtn), true);
  assert.equal(dispatchClick(innerSpan), true);
  assert.equal(dispatchClick(legendBtn), true);
  assert.equal(seen.length, 3);
  assert.equal(seen[0].submitter, defaultBtn);
  assert.equal(seen[0].defaultPrevented, true);
  assert.equal(seen[1].submitter, submitBtn);
  assert.equal(seen[1].defaultPrevented, true);
  assert.equal(seen[2].submitter, legendBtn);
  assert.equal(seen[2].defaultPrevented, true);

  assert.equal(dispatchClick(typeBtn), false);
  assert.equal(dispatchClick(disabledBtn), false);
  assert.equal(dispatchClick(fsBtn), false);
  assert.equal(dispatchClick(formActionBtn), false);
  assert.equal(dispatchClick(explicitBtn), false);

  cleanup();
  awaitableTeardown(win);
});

test("installSandboxFormActions synthesizes submit on Enter in single-line inputs and respects constraints", () => {
  const { win, doc } = setupDoc();
  const cleanup = installSandboxFormActions(doc);

  const form = doc.createElement("form");
  const input = doc.createElement("input");
  input.type = "text";
  input.required = true;
  const btn = doc.createElement("button");
  btn.type = "submit";
  form.append(input, btn);

  const singleForm = doc.createElement("form");
  const singleInput = doc.createElement("input");
  singleForm.append(singleInput);

  const twoForm = doc.createElement("form");
  const aInput = doc.createElement("input");
  const bInput = doc.createElement("input");
  twoForm.append(aInput, bInput);

  doc.body.append(form, singleForm, twoForm);

  const submits = [];
  form.addEventListener("submit", (e) => submits.push(e.submitter));
  let singleSubmits = 0;
  singleForm.addEventListener("submit", () => { singleSubmits += 1; });
  let twoSubmits = 0;
  twoForm.addEventListener("submit", () => { twoSubmits += 1; });

  input.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
  assert.equal(submits.length, 0);

  input.value = "ok";
  input.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
  assert.equal(submits.length, 1);
  assert.equal(submits[0], btn);

  btn.disabled = true;
  input.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
  assert.equal(submits.length, 1);

  singleInput.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
  assert.equal(singleSubmits, 1);
  aInput.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
  assert.equal(twoSubmits, 0);

  cleanup();
  awaitableTeardown(win);
});

test("installSandboxFormActions is ref-counted and idempotent per Document", () => {
  const { win, doc } = setupDoc();
  const form = doc.createElement("form");
  const btn = doc.createElement("button");
  btn.type = "submit";
  form.append(btn);
  doc.body.append(form);

  let count = 0;
  form.addEventListener("submit", () => { count += 1; });

  const c1 = installSandboxFormActions(doc);
  const c2 = installSandboxFormActions(doc);

  const firstClick = new win.MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
  btn.dispatchEvent(firstClick);
  assert.equal(firstClick.defaultPrevented, true);
  assert.equal(count, 1, "multiple installs on same document must still submit once per click");

  c1();
  c1();
  const secondClick = new win.MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
  btn.dispatchEvent(secondClick);
  assert.equal(secondClick.defaultPrevented, true);
  assert.equal(count, 2, "remains active until all installers clean up");

  c2();
  const afterClick = new win.MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
  btn.dispatchEvent(afterClick);
  assert.equal(afterClick.defaultPrevented, false, "click handler detaches once last installer cleans up");

  const afterSubmit = new win.Event("submit", { bubbles: true, cancelable: true });
  form.dispatchEvent(afterSubmit);
  assert.equal(afterSubmit.defaultPrevented, false, "submit handler detaches once last installer cleans up");

  awaitableTeardown(win);
});
