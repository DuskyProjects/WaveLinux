import { ArrowDown, Check } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";

const SELECT_VISIBLE_OPTION_LIMIT = 80;

export type SelectOption = {
  value: string;
  label: string;
  disabled?: boolean;
};

export function AppSelect({
  ariaLabel,
  className = "",
  disabled = false,
  id,
  onChange,
  options,
  value,
}: {
  ariaLabel: string;
  className?: string;
  disabled?: boolean;
  id?: string;
  onChange: (value: string) => void;
  options: SelectOption[];
  value: string;
}) {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const buttonRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const selectSearchOnFocusRef = useRef(true);
  const [open, setOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const [position, setPosition] = useState({ left: 0, top: 0, width: 360, maxHeight: 260 });
  const selectedIndex = options.findIndex((option) => option.value === value);
  const selectedOption = selectedIndex >= 0 ? options[selectedIndex] : options[0];
  const filteredOptions = useMemo(
    () => filterSelectOptions(options, searchQuery),
    [options, searchQuery],
  );
  const visibleOptions = useMemo(
    () => visibleSelectOptions(filteredOptions, value),
    [filteredOptions, value],
  );

  const positionMenu = useCallback(() => {
    const button = buttonRef.current;
    if (!button) return;
    const rect = button.getBoundingClientRect();
    const viewportMargin = 12;
    const left = Math.max(
      viewportMargin,
      Math.min(rect.left, window.innerWidth - viewportMargin - 240),
    );
    const availableRight = Math.max(240, window.innerWidth - left - viewportMargin);
    const readableWidth = Math.min(520, availableRight, Math.max(360, rect.width));
    const maxHeight = Math.min(320, Math.max(140, window.innerHeight - viewportMargin * 2));
    const height = Math.min(
      maxHeight,
      Math.max(140, window.innerHeight - rect.top - viewportMargin),
    );
    setPosition({
      left,
      top: Math.max(
        viewportMargin,
        Math.min(rect.top, window.innerHeight - viewportMargin - height),
      ),
      width: readableWidth,
      maxHeight: height,
    });
  }, []);

  const openMenu = useCallback(
    (initialSearch = "") => {
      if (disabled || options.length === 0) return;
      const nextOptions = visibleSelectOptions(
        filterSelectOptions(options, initialSearch),
        value,
      );
      const nextSelectedIndex = nextOptions.findIndex((option) => option.value === value);
      selectSearchOnFocusRef.current = initialSearch.length === 0;
      setSearchQuery(initialSearch);
      setActiveIndex(
        nextSelectedIndex >= 0 ? nextSelectedIndex : firstEnabledOptionIndex(nextOptions),
      );
      positionMenu();
      setOpen(true);
    },
    [disabled, options, positionMenu, value],
  );

  const closeMenu = useCallback(() => {
    setOpen(false);
    setSearchQuery("");
  }, []);

  useEffect(() => {
    if (!open) return undefined;
    positionMenu();
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (
        target &&
        (rootRef.current?.contains(target) || menuRef.current?.contains(target))
      ) {
        return;
      }
      closeMenu();
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") closeMenu();
    };
    const onScroll = (event: Event) => {
      const target = event.target as Node | null;
      if (target && menuRef.current?.contains(target)) return;
      closeMenu();
    };
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("resize", positionMenu);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("resize", positionMenu);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [closeMenu, open, positionMenu]);

  useEffect(() => {
    if (!open) return undefined;
    const frame = window.requestAnimationFrame(() => {
      searchRef.current?.focus({ preventScroll: true });
      if (selectSearchOnFocusRef.current) {
        searchRef.current?.select();
      } else {
        const length = searchRef.current?.value.length ?? 0;
        searchRef.current?.setSelectionRange(length, length);
      }
    });
    return () => window.cancelAnimationFrame(frame);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    setActiveIndex((current) => {
      if (visibleOptions[current] && !visibleOptions[current].disabled) return current;
      return firstEnabledOptionIndex(visibleOptions);
    });
  }, [open, visibleOptions]);

  const choose = useCallback(
    (option: SelectOption) => {
      if (option.disabled) return;
      closeMenu();
      if (option.value !== value) onChange(option.value);
      buttonRef.current?.focus({ preventScroll: true });
    },
    [closeMenu, onChange, value],
  );

  const moveActive = useCallback(
    (direction: 1 | -1) => {
      setActiveIndex((current) => nextEnabledOptionIndex(visibleOptions, current, direction));
    },
    [visibleOptions],
  );

  const chooseActive = useCallback(() => {
    const option = visibleOptions[activeIndex];
    if (option) choose(option);
  }, [activeIndex, choose, visibleOptions]);

  const handleSearchKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      moveActive(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      moveActive(-1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      chooseActive();
    } else if (event.key === "Escape") {
      event.preventDefault();
      closeMenu();
      buttonRef.current?.focus({ preventScroll: true });
    }
  };

  return (
    <div
      className={["app-select", className].filter(Boolean).join(" ")}
      onClick={(event) => event.stopPropagation()}
      onPointerDown={(event) => event.stopPropagation()}
      ref={rootRef}
    >
      <button
        aria-expanded={open}
        aria-haspopup="listbox"
        aria-label={ariaLabel}
        className="app-select-button"
        disabled={disabled}
        id={id}
        ref={buttonRef}
        onClick={() => (open ? closeMenu() : openMenu())}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown") {
            event.preventDefault();
            if (!open) openMenu();
            else moveActive(1);
          } else if (event.key === "ArrowUp") {
            event.preventDefault();
            if (!open) openMenu();
            else moveActive(-1);
          } else if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            if (open) chooseActive();
            else openMenu();
          } else if (!open && isPrintableSelectSearchKey(event)) {
            event.preventDefault();
            openMenu(event.key);
          }
        }}
        type="button"
      >
        <span>{selectedOption?.label ?? "Select"}</span>
        <ArrowDown size={15} />
      </button>
      {open &&
        typeof document !== "undefined" &&
        createPortal(
          <div
            className="app-select-menu"
            ref={menuRef}
            style={{
              left: position.left,
              maxHeight: position.maxHeight,
              top: position.top,
              width: position.width,
            }}
          >
            <input
              aria-label={`${ariaLabel} search`}
              className="app-select-search"
              onChange={(event) => setSearchQuery(event.currentTarget.value)}
              onKeyDown={handleSearchKeyDown}
              placeholder="Search"
              ref={searchRef}
              value={searchQuery}
            />
            <div className="app-select-options" role="listbox">
              {visibleOptions.map((option, index) => (
                <button
                  aria-selected={option.value === value}
                  className={[
                    "app-select-option",
                    option.value === value ? "selected" : "",
                    index === activeIndex ? "active" : "",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  disabled={option.disabled}
                  key={`${option.value}-${index}`}
                  onClick={() => choose(option)}
                  role="option"
                  title={option.label}
                  type="button"
                >
                  <span>{option.label}</span>
                  {option.value === value && <Check size={14} />}
                </button>
              ))}
              {filteredOptions.length === 0 && (
                <div className="app-select-empty">No matching options</div>
              )}
              {filteredOptions.length > visibleOptions.length && (
                <div className="app-select-empty">
                  Showing {visibleOptions.length} of {filteredOptions.length}
                </div>
              )}
            </div>
          </div>,
          document.body,
        )}
    </div>
  );
}

function firstEnabledOptionIndex(options: SelectOption[]) {
  const index = options.findIndex((option) => !option.disabled);
  return index >= 0 ? index : 0;
}

function nextEnabledOptionIndex(
  options: SelectOption[],
  current: number,
  direction: 1 | -1,
) {
  if (options.length === 0) return 0;
  let next = current;
  for (let attempt = 0; attempt < options.length; attempt += 1) {
    next = (next + direction + options.length) % options.length;
    if (!options[next]?.disabled) return next;
  }
  return current;
}

function filterSelectOptions(options: SelectOption[], query: string): SelectOption[] {
  const needles = normalizeSelectSearch(query).split(" ").filter(Boolean);
  if (needles.length === 0) return options;
  return options.filter((option) => {
    const haystack = normalizeSelectSearch(`${option.label} ${option.value}`);
    return needles.every((needle) => haystack.includes(needle));
  });
}

function visibleSelectOptions(
  options: SelectOption[],
  selectedValue: string,
): SelectOption[] {
  if (options.length <= SELECT_VISIBLE_OPTION_LIMIT) return options;
  const visible = options.slice(0, SELECT_VISIBLE_OPTION_LIMIT);
  if (!selectedValue || visible.some((option) => option.value === selectedValue)) {
    return visible;
  }
  const selectedOption = options.find((option) => option.value === selectedValue);
  return selectedOption
    ? [selectedOption, ...visible.slice(0, SELECT_VISIBLE_OPTION_LIMIT - 1)]
    : visible;
}

function normalizeSelectSearch(value: string): string {
  return value.trim().toLowerCase();
}

function isPrintableSelectSearchKey(event: ReactKeyboardEvent<HTMLElement>): boolean {
  return event.key.length === 1 && !event.altKey && !event.ctrlKey && !event.metaKey;
}
