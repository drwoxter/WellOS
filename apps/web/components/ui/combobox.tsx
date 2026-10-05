"use client";

import { useEffect, useId, useMemo, useRef, useState } from "react";
import { Icon } from "./icons";

export type ComboOption = {
  id: string;
  label: string;
  sub?: string;
  keywords?: string;
};

/**
 * Searchable single-select combobox (WAI-ARIA 1.2 pattern): typeahead
 * filtering, ArrowUp/Down, Home/End, Enter to select, Escape to close,
 * optional async loading via `onQuery`. The selected option is announced
 * through `aria-activedescendant`; no action depends on hover.
 */
export function Combobox({
  id,
  label,
  placeholder,
  options,
  value,
  onChange,
  onQuery,
  loading,
  emptyText,
  loadingText,
  clearLabel,
  disabled,
  required,
  describedBy,
  minChars = 0,
}: {
  id?: string;
  label: string;
  placeholder?: string;
  options: ComboOption[];
  value: ComboOption | null;
  onChange: (next: ComboOption | null) => void;
  onQuery?: (q: string) => void;
  loading?: boolean;
  emptyText: string;
  loadingText: string;
  clearLabel: string;
  disabled?: boolean;
  required?: boolean;
  describedBy?: string;
  minChars?: number;
}) {
  const auto = useId();
  const inputId = id ?? `combo-${auto}`;
  const listId = `${inputId}-list`;
  const [query, setQuery] = useState(value?.label ?? "");
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const rootRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const lastValue = useRef<ComboOption | null>(value);
  useEffect(() => {
    const prev = lastValue.current;
    lastValue.current = value;
    if (value) setQuery(value.label);
    // A selection cleared from outside empties the field; one cleared
    // because the user typed over it keeps what they typed.
    else if (prev) setQuery((q) => (q === prev.label ? "" : q));
  }, [value]);

  const filtered = useMemo(() => {
    if (onQuery) return options;
    const q = query.trim().toLowerCase();
    if (!q) return options;
    return options.filter((o) =>
      `${o.label} ${o.sub ?? ""} ${o.keywords ?? ""}`.toLowerCase().includes(q),
    );
  }, [options, query, onQuery]);

  useEffect(() => {
    if (active >= filtered.length) setActive(0);
  }, [filtered.length, active]);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  const pick = (o: ComboOption) => {
    onChange(o);
    setQuery(o.label);
    setOpen(false);
  };

  const showList = open && query.trim().length >= minChars;
  const activeId =
    showList && filtered[active]
      ? `${listId}-${filtered[active].id}`
      : undefined;

  return (
    <div className="combobox" ref={rootRef}>
      <label htmlFor={inputId}>{label}</label>
      <div className="combobox-input-wrap">
        <Icon.Search />
        <input
          ref={inputRef}
          id={inputId}
          type="text"
          role="combobox"
          autoComplete="off"
          aria-autocomplete="list"
          aria-expanded={showList}
          aria-controls={listId}
          aria-activedescendant={activeId}
          aria-describedby={describedBy}
          aria-required={required || undefined}
          placeholder={placeholder}
          disabled={disabled}
          value={query}
          onChange={(e) => {
            const q = e.target.value;
            setQuery(q);
            setOpen(true);
            setActive(0);
            if (value && q !== value.label) onChange(null);
            onQuery?.(q);
          }}
          onFocus={() => setOpen(true)}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setOpen(true);
              setActive((a) =>
                Math.min(a + 1, Math.max(filtered.length - 1, 0)),
              );
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setActive((a) => Math.max(a - 1, 0));
            } else if (e.key === "Home" && showList) {
              e.preventDefault();
              setActive(0);
            } else if (e.key === "End" && showList) {
              e.preventDefault();
              setActive(Math.max(filtered.length - 1, 0));
            } else if (e.key === "Enter") {
              if (showList && filtered[active]) {
                e.preventDefault();
                pick(filtered[active]);
              }
            } else if (e.key === "Escape") {
              if (open) {
                e.preventDefault();
                setOpen(false);
              }
            }
          }}
        />
        {query && !disabled ? (
          <button
            type="button"
            className="ghost combobox-clear"
            aria-label={clearLabel}
            onClick={() => {
              setQuery("");
              onChange(null);
              onQuery?.("");
              setOpen(true);
              inputRef.current?.focus();
            }}
          >
            <Icon.Close />
          </button>
        ) : null}
      </div>
      <ul
        id={listId}
        role="listbox"
        aria-label={label}
        className="combobox-list"
        hidden={!showList}
      >
        {loading ? (
          <li
            className="combobox-empty"
            role="option"
            aria-selected={false}
            aria-disabled="true"
          >
            {loadingText}
          </li>
        ) : filtered.length === 0 ? (
          <li
            className="combobox-empty"
            role="option"
            aria-selected={false}
            aria-disabled="true"
          >
            {emptyText}
          </li>
        ) : (
          filtered.map((o, i) => (
            <li
              key={o.id}
              id={`${listId}-${o.id}`}
              role="option"
              aria-selected={i === active}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => pick(o)}
              onMouseMove={() => setActive(i)}
            >
              <span>{o.label}</span>
              {o.sub ? <span className="option-sub">{o.sub}</span> : null}
            </li>
          ))
        )}
      </ul>
    </div>
  );
}
