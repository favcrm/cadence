import {
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { IconCheck, IconChevron } from "./icons";
import {
  closedSelect,
  filterOptions,
  flipPlacement,
  openSelect,
  optionBadgeText,
  reduceOutside,
  reducePick,
  reduceQuery,
  reduceSelectKey,
  showsSearch,
  type SelectModel,
  type SelectOption,
} from "./selectLogic";

export type { SelectOption };

export type SelectProps = {
  value: string;
  /** Commits the option's `value` string. */
  onChange: (value: string) => void;
  options: SelectOption[];
  placeholder?: string;
  /** Force the search field. It also appears on its own above 8 options. */
  searchable?: boolean;
  disabled?: boolean;
  id?: string;
  /** `sm` is 1.75rem, `md` is 2rem — the same scale as Button. */
  size?: "sm" | "md";
  full?: boolean;
  className?: string;
  title?: string;
  "aria-label"?: string;
};

/**
 * Select-only combobox: a button trigger plus a portaled listbox.
 * Keyboard: Up/Down/Home/End move, Enter/Space pick, Escape closes,
 * and a typed prefix jumps (unless the search field is showing).
 */
export function Select({
  value,
  onChange,
  options,
  placeholder,
  searchable,
  disabled,
  id,
  size = "md",
  full,
  className,
  title,
  ...rest
}: SelectProps) {
  const ariaLabel = rest["aria-label"];
  const reactId = useId();
  const base = id ?? reactId;
  const listId = `${base}-listbox`;
  const [model, setModel] = useState<SelectModel>(closedSelect);
  const modelRef = useRef(model);
  modelRef.current = model;
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const keyClick = useRef(false);
  // Set when a close should return focus to the trigger after the portal
  // unmounts. Tab leaves it clear so the browser can move to the next control.
  const restoreTrigger = useRef(false);
  const searchOn = showsSearch(options, searchable);
  const list = filterOptions(options, searchOn ? model.query : "");
  const selected = options.find((option) => option.value === value);
  const activeId = model.open && model.active >= 0 ? `${base}-opt-${model.active}` : undefined;

  const apply = (nextModel: SelectModel) => {
    modelRef.current = nextModel;
    setModel(nextModel);
  };

  const finish = (pick: string | null, nextModel: SelectModel, restore: boolean) => {
    const wasOpen = modelRef.current.open;
    apply(nextModel);
    if (pick !== null) onChange(pick);
    if (restore && wasOpen && !nextModel.open) restoreTrigger.current = true;
  };

  const onListKey = (event: ReactKeyboardEvent) => {
    if (disabled) return;
    const current = modelRef.current;
    const result = reduceSelectKey(current, options, searchOn, value, event.key, Date.now(), {
      meta: event.metaKey,
      ctrl: event.ctrlKey,
      alt: event.altKey,
    });
    if (result.prevent) event.preventDefault();
    if (result.model !== current || result.pick !== null) {
      const closing = current.open && !result.model.open;
      finish(result.pick, result.model, closing && event.key !== "Tab");
    }
  };

  useLayoutEffect(() => {
    if (model.open || !restoreTrigger.current) return;
    restoreTrigger.current = false;
    triggerRef.current?.focus();
  }, [model.open]);

  useEffect(() => {
    if (!model.open) return;
    const onPointer = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (triggerRef.current?.contains(target) || popRef.current?.contains(target)) return;
      finish(null, reduceOutside(modelRef.current), true);
    };
    document.addEventListener("pointerdown", onPointer);
    return () => document.removeEventListener("pointerdown", onPointer);
  }, [model.open]);

  useEffect(() => {
    if (!model.open || !searchOn) return;
    searchRef.current?.focus();
  }, [model.open, searchOn]);

  useEffect(() => {
    if (!model.open || model.active < 0) return;
    document.getElementById(`${base}-opt-${model.active}`)?.scrollIntoView({ block: "nearest" });
  }, [model.open, model.active, base]);

  useLayoutEffect(() => {
    if (!model.open) return;
    const place = () => {
      const trigger = triggerRef.current;
      const pop = popRef.current;
      if (!trigger || !pop) return;
      const rect = trigger.getBoundingClientRect();
      const gap = 4;
      const width = Math.max(rect.width, 240);
      const placement = flipPlacement(rect, pop.offsetHeight, window.innerHeight, gap);
      let left = rect.left;
      const maxLeft = window.innerWidth - width - 8;
      if (left > maxLeft) left = Math.max(8, maxLeft);
      const top = placement === "below" ? rect.bottom + gap : Math.max(8, rect.top - gap - pop.offsetHeight);
      pop.style.top = `${top}px`;
      pop.style.left = `${left}px`;
      pop.style.width = `${width}px`;
      pop.dataset.placement = placement;
    };
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [model.open, model.query, list.length]);

  let lastGroup: string | undefined;
  const rows: ReactNode[] = [];
  list.forEach((option, index) => {
    if (option.group && option.group !== lastGroup) {
      rows.push(
        <li key={`group-${option.group}-${index}`} role="presentation" className="select-group">
          {option.group}
        </li>,
      );
    }
    lastGroup = option.group;
    const badge = optionBadgeText(option);
    const active = index === model.active;
    rows.push(
      <li
        key={`${option.value}-${index}`}
        id={`${base}-opt-${index}`}
        role="option"
        aria-selected={option.value === value}
        aria-disabled={option.disabled || undefined}
        data-active={active || undefined}
        className="select-opt"
        onMouseEnter={() => {
          if (!option.disabled) apply({ ...modelRef.current, active: index });
        }}
        onClick={() => {
          const result = reducePick(modelRef.current, option);
          finish(result.pick, result.model, result.pick !== null);
        }}
      >
        <span className="select-check" aria-hidden="true">
          {option.value === value ? <IconCheck size={12} /> : null}
        </span>
        <span className="select-copy">
          <span className="select-label">{option.label}</span>
          {option.hint ? <span className="select-hint">{option.hint}</span> : null}
        </span>
        {badge ? <span className="select-badge">{badge}</span> : null}
      </li>,
    );
  });

  const pop =
    model.open && typeof document !== "undefined"
      ? createPortal(
          <div className="select-pop" ref={popRef} data-placement="below">
            {searchOn ? (
              <input
                ref={searchRef}
                className="select-search"
                value={model.query}
                placeholder="Search"
                aria-label="Search options"
                aria-controls={listId}
                aria-activedescendant={activeId}
                onChange={(event) => apply(reduceQuery(modelRef.current, options, event.target.value, value))}
                onKeyDown={(event) => {
                  if (
                    event.key === "ArrowDown" ||
                    event.key === "ArrowUp" ||
                    event.key === "Home" ||
                    event.key === "End" ||
                    event.key === "Enter" ||
                    event.key === "Escape" ||
                    event.key === "Tab"
                  ) {
                    onListKey(event);
                  }
                }}
              />
            ) : null}
            <ul
              id={listId}
              role="listbox"
              aria-label={ariaLabel}
              tabIndex={-1}
              className="select-list"
              onKeyDown={onListKey}
            >
              {rows.length > 0 ? (
                rows
              ) : (
                <li role="presentation" className="select-empty">
                  No matches
                </li>
              )}
            </ul>
          </div>,
          document.body,
        )
      : null;

  return (
    <div className={["select", size === "sm" ? "select-sm" : "", full ? "select-full" : "", className ?? ""].filter(Boolean).join(" ")}>
      <button
        ref={triggerRef}
        id={id}
        type="button"
        className="select-trigger"
        role="combobox"
        aria-haspopup="listbox"
        aria-expanded={model.open}
        aria-controls={listId}
        aria-activedescendant={activeId}
        aria-label={ariaLabel}
        title={title}
        disabled={disabled}
        onClick={() => {
          // Enter/Space already ran the listbox key handler and the browser
          // then synthesizes a click. Ignore that second activation.
          if (keyClick.current) {
            keyClick.current = false;
            return;
          }
          if (disabled) return;
          const current = modelRef.current;
          if (current.open) finish(null, closedSelect(), true);
          else apply(openSelect(options, value));
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            keyClick.current = true;
            setTimeout(() => {
              keyClick.current = false;
            }, 0);
          }
          onListKey(event);
        }}
      >
        <span className={selected ? "select-value" : "select-value select-placeholder"}>
          {selected?.label ?? placeholder ?? "Select"}
        </span>
        <IconChevron size={12} className="select-caret" />
      </button>
      {pop}
    </div>
  );
}

export default Select;
