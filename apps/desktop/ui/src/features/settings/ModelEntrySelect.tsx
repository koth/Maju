import { Check, ChevronDown } from "lucide-react";
import {
  forwardRef,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";

export type ModelEntrySelectOption<T extends string> = {
  value: T;
  label: string;
};

type ModelEntrySelectProps<T extends string> = {
  value: T;
  options: ModelEntrySelectOption<T>[];
  onChange: (value: T) => void;
  disabled?: boolean;
  "aria-label": string;
};

function ModelEntrySelectInner<T extends string>(
  { value, options, onChange, disabled, ...rest }: ModelEntrySelectProps<T>,
  ref: React.ForwardedRef<HTMLButtonElement>,
) {
  const ariaLabel = rest["aria-label"];
  const listboxId = useId();
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const listboxRef = useRef<HTMLUListElement | null>(null);
  const [open, setOpen] = useState(false);

  const setRefs = useCallback(
    (node: HTMLButtonElement | null) => {
      triggerRef.current = node;
      if (typeof ref === "function") ref(node);
      else if (ref) {
        (ref as React.MutableRefObject<HTMLButtonElement | null>).current = node;
      }
    },
    [ref],
  );

  const selectedIndex = options.findIndex((option) => option.value === value);
  const selected = selectedIndex >= 0 ? options[selectedIndex] : undefined;

  const pick = useCallback(
    (next: T) => {
      onChange(next);
      setOpen(false);
      triggerRef.current?.focus();
    },
    [onChange],
  );

  useEffect(() => {
    if (!open) return;
    const onDocPointer = (event: MouseEvent) => {
      if (
        triggerRef.current?.contains(event.target as Node) ||
        listboxRef.current?.contains(event.target as Node)
      ) {
        return;
      }
      setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpen(false);
        triggerRef.current?.focus();
      }
    };
    document.addEventListener("mousedown", onDocPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const onTriggerKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>) => {
    if (disabled) return;
    if (event.key === "ArrowDown" || event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      setOpen(true);
    }
  };

  const onListKeyDown = (event: React.KeyboardEvent<HTMLUListElement>) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      const next = options[(selectedIndex + 1) % options.length];
      if (next) pick(next.value);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      const prev =
        options[(selectedIndex - 1 + options.length) % options.length];
      if (prev) pick(prev.value);
    } else if (event.key === "Home") {
      event.preventDefault();
      const first = options[0];
      if (first) pick(first.value);
    } else if (event.key === "End") {
      event.preventDefault();
      const last = options[options.length - 1];
      if (last) pick(last.value);
    } else if (event.key === "Tab" || event.key === "Escape") {
      setOpen(false);
      triggerRef.current?.focus();
    }
  };

  return (
    <>
      <button
        ref={setRefs}
        type="button"
        aria-label={ariaLabel}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listboxId}
        disabled={disabled}
        className="settings-model-entry-select"
        onClick={() => !disabled && setOpen((prev) => !prev)}
        onKeyDown={onTriggerKeyDown}
      >
        <span>{selected?.label ?? ""}</span>
        <ChevronDown
          size={14}
          aria-hidden
          className="settings-model-entry-select-icon"
        />
      </button>
      {open && (
        <ul
          ref={listboxRef}
          id={listboxId}
          role="listbox"
          tabIndex={-1}
          aria-label={ariaLabel}
          className="settings-model-entry-select-content"
          onKeyDown={onListKeyDown}
        >
          {options.map((option) => {
            const isSelected = option.value === value;
            return (
              <li
                key={option.value}
                role="option"
                aria-selected={isSelected}
                data-option-value={option.value}
                tabIndex={-1}
                className="settings-model-entry-select-item"
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => pick(option.value)}
              >
                <span>{option.label}</span>
                {isSelected && (
                  <span className="settings-model-entry-select-indicator">
                    <Check size={12} aria-hidden />
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </>
  );
}

export const ModelEntrySelect = forwardRef(ModelEntrySelectInner) as <
  T extends string,
>(
  props: ModelEntrySelectProps<T> & { ref?: React.Ref<HTMLButtonElement> },
) => ReturnType<typeof ModelEntrySelectInner>;
