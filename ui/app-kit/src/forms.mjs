const installedDocs = new WeakMap();

const IMPLICIT_SUBMIT_INPUT_TYPES = new Set([
  "",
  "text",
  "search",
  "url",
  "tel",
  "email",
  "password",
  "date",
  "month",
  "week",
  "time",
  "datetime-local",
  "number",
]);

function tagOf(node) {
  return typeof node?.tagName === "string" ? node.tagName.toUpperCase() : "";
}

function isActionlessForm(form) {
  if (tagOf(form) !== "FORM" || typeof form.getAttribute !== "function") return false;
  const rawAction = form.getAttribute("action");
  return rawAction === null || rawAction.trim() === "";
}

function hasExplicitFormAction(submitter) {
  if (!submitter || typeof submitter.getAttribute !== "function") return false;
  const raw = submitter.getAttribute("formaction");
  return raw !== null && raw.trim() !== "";
}

function resolveForm(doc, control) {
  if (!control || typeof control.getAttribute !== "function") return null;
  const formAttr = control.getAttribute("form");
  if (formAttr !== null) {
    const id = formAttr.trim();
    if (!id || typeof doc.getElementById !== "function") return null;
    const target = doc.getElementById(id);
    return tagOf(target) === "FORM" ? target : null;
  }
  if (tagOf(control.form) === "FORM") return control.form;
  let cur = control.parentElement ?? null;
  while (cur) {
    if (tagOf(cur) === "FORM") return cur;
    cur = cur.parentElement ?? null;
  }
  return null;
}

function isDisabledFieldset(fieldset) {
  return fieldset.disabled === true || (typeof fieldset.hasAttribute === "function" && fieldset.hasAttribute("disabled"));
}

function isInFirstLegend(control, fieldset) {
  let firstLegend = null;
  const children = fieldset.children ?? [];
  for (let i = 0; i < children.length; i++) {
    if (tagOf(children[i]) === "LEGEND") {
      firstLegend = children[i];
      break;
    }
  }
  if (!firstLegend) return false;
  let cur = control;
  while (cur && cur !== fieldset) {
    if (cur === firstLegend) return true;
    cur = cur.parentElement ?? null;
  }
  return false;
}

function isDisabledInForm(control, _form) {
  if (!control) return true;
  if (control.disabled === true || (typeof control.hasAttribute === "function" && control.hasAttribute("disabled"))) {
    return true;
  }
  let cur = control.parentElement ?? null;
  while (cur) {
    if (tagOf(cur) === "FIELDSET" && isDisabledFieldset(cur) && !isInFirstLegend(control, cur)) {
      return true;
    }
    cur = cur.parentElement ?? null;
  }
  return false;
}

function isSubmitButton(el) {
  const tag = tagOf(el);
  const rawType = typeof el?.getAttribute === "function" ? el.getAttribute("type") : el?.type;
  const normalized = typeof rawType === "string" ? rawType.trim().toLowerCase() : "";
  if (tag === "BUTTON") {
    return normalized !== "button" && normalized !== "reset";
  }
  if (tag === "INPUT") {
    return normalized === "submit" || normalized === "image";
  }
  return false;
}

function isImplicitSubmitInput(el) {
  if (tagOf(el) !== "INPUT") return false;
  const rawType = typeof el.getAttribute === "function" ? el.getAttribute("type") : el.type;
  const normalized = typeof rawType === "string" ? rawType.trim().toLowerCase() : "";
  return IMPLICIT_SUBMIT_INPUT_TYPES.has(normalized);
}

function passesConstraintValidation(form, submitter) {
  if (
    form.noValidate === true ||
    (typeof form.hasAttribute === "function" && form.hasAttribute("novalidate")) ||
    submitter?.formNoValidate === true ||
    (typeof submitter?.hasAttribute === "function" && submitter.hasAttribute("formnovalidate"))
  ) {
    return true;
  }
  if (typeof form.reportValidity === "function") return form.reportValidity() !== false;
  if (typeof form.checkValidity === "function") return form.checkValidity() !== false;
  return true;
}

function dispatchSyntheticSubmit(doc, form, submitter) {
  const view = doc.defaultView ?? globalThis;
  const init = { bubbles: true, cancelable: true, submitter: submitter ?? null };
  let event;
  if (typeof view.SubmitEvent === "function") {
    try {
      event = new view.SubmitEvent("submit", init);
    } catch {
      event = null;
    }
  }
  if (!event) {
    const EventCtor = view.Event ?? Event;
    event = new EventCtor("submit", { bubbles: true, cancelable: true });
    if (event.submitter === undefined) {
      try {
        Object.defineProperty(event, "submitter", {
          value: submitter ?? null,
          configurable: true,
          enumerable: true,
        });
      } catch {
        // Ignore non-configurable submitter in strict Event implementations
      }
    }
  }
  return form.dispatchEvent(event);
}

function collectAssociatedControls(doc, form) {
  const seen = new Set();
  const controls = [];
  const add = (el) => {
    if (el && !seen.has(el)) {
      seen.add(el);
      controls.push(el);
    }
  };
  const nativeElements = form.elements ?? form.querySelectorAll?.("button, input") ?? [];
  for (const el of Array.from(nativeElements)) add(el);
  if (typeof form.id === "string" && form.id.trim() !== "" && typeof doc.querySelectorAll === "function") {
    for (const el of Array.from(doc.querySelectorAll("button[form], input[form]"))) {
      if (resolveForm(doc, el) === form) add(el);
    }
  }
  return controls;
}

function findClickSubmitter(target) {
  let node = tagOf(target) ? target : target?.parentElement ?? null;
  while (node) {
    const tag = tagOf(node);
    if (isSubmitButton(node)) return node;
    if (
      tag === "BUTTON" ||
      tag === "INPUT" ||
      tag === "SELECT" ||
      tag === "TEXTAREA" ||
      tag === "FORM" ||
      (tag === "A" && typeof node.hasAttribute === "function" && node.hasAttribute("href"))
    ) {
      return null;
    }
    node = node.parentElement ?? null;
  }
  return null;
}

/**
 * Installs idempotent per-Document form submission handling for sandboxed
 * `<iframe sandbox="allow-scripts">` screens (where `allow-forms` is omitted):
 * prevents native navigation on actionless forms during `submit` capture and
 * synthesizes cancelable `submit` events on submit-button clicks and `Enter`.
 */
export function installSandboxFormActions(doc = globalThis.document) {
  if (!doc || typeof doc.addEventListener !== "function") return () => {};

  let entry = installedDocs.get(doc);
  if (!entry) {
    const onSubmit = (event) => {
      if (!event || event.defaultPrevented) return;
      if (!hasExplicitFormAction(event.submitter) && isActionlessForm(event.target)) {
        if (typeof event.preventDefault === "function") event.preventDefault();
      }
    };

    const onClick = (event) => {
      if (!event || event.defaultPrevented) return;
      if (typeof event.button === "number" && event.button !== 0) return;
      const submitter = findClickSubmitter(event.target);
      if (!submitter || hasExplicitFormAction(submitter)) return;
      const form = resolveForm(doc, submitter);
      if (!isActionlessForm(form) || isDisabledInForm(submitter, form)) return;
      if (typeof event.preventDefault === "function") event.preventDefault();
      if (!passesConstraintValidation(form, submitter)) return;
      dispatchSyntheticSubmit(doc, form, submitter);
    };

    const onKeyDown = (event) => {
      if (
        !event ||
        event.defaultPrevented ||
        event.key !== "Enter" ||
        event.isComposing ||
        event.shiftKey ||
        event.ctrlKey ||
        event.altKey ||
        event.metaKey
      ) {
        return;
      }
      const target = event.target;
      if (!isImplicitSubmitInput(target)) return;
      const form = resolveForm(doc, target);
      if (!isActionlessForm(form) || isDisabledInForm(target, form)) return;

      const controls = collectAssociatedControls(doc, form);
      let defaultSubmitter = null;
      for (const control of controls) {
        if (isSubmitButton(control) && resolveForm(doc, control) === form) {
          defaultSubmitter = control;
          break;
        }
      }

      if (defaultSubmitter) {
        if (isDisabledInForm(defaultSubmitter, form) || hasExplicitFormAction(defaultSubmitter)) return;
      } else {
        let implicitCount = 0;
        for (const control of controls) {
          if (
            isImplicitSubmitInput(control) &&
            resolveForm(doc, control) === form &&
            !isDisabledInForm(control, form)
          ) {
            implicitCount += 1;
            if (implicitCount > 1) return;
          }
        }
      }

      if (typeof event.preventDefault === "function") event.preventDefault();
      if (!passesConstraintValidation(form, defaultSubmitter)) return;
      dispatchSyntheticSubmit(doc, form, defaultSubmitter);
    };

    doc.addEventListener("submit", onSubmit, { capture: true });
    doc.addEventListener("click", onClick);
    doc.addEventListener("keydown", onKeyDown);

    entry = {
      count: 0,
      detach: () => {
        if (typeof doc.removeEventListener === "function") {
          doc.removeEventListener("submit", onSubmit, { capture: true });
          doc.removeEventListener("click", onClick);
          doc.removeEventListener("keydown", onKeyDown);
        }
        installedDocs.delete(doc);
      },
    };
    installedDocs.set(doc, entry);
  }

  entry.count += 1;
  let active = true;
  return () => {
    if (!active) return;
    active = false;
    entry.count -= 1;
    if (entry.count <= 0) {
      entry.detach();
    }
  };
}
