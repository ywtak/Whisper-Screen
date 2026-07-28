/**
 * The small form controls every settings tab shares.
 *
 * The window is 460 px wide and every tab is a stack of labelled inputs, so
 * the label/control/hint pattern lives here once instead of being re-rolled
 * per tab. No controlled-string gymnastics: numeric fields reject NaN at the
 * boundary and always hand back a number.
 */

import type { ReactNode } from "react";

export function Field({ label, hint, children }: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <label className="field">
      <span>{label}</span>
      {children}
      {hint && <em>{hint}</em>}
    </label>
  );
}

export function TextField({ label, value, placeholder, hint, mono, onChange }: {
  label: string;
  value: string;
  placeholder?: string;
  hint?: string;
  mono?: boolean;
  onChange: (value: string) => void;
}) {
  return (
    <Field label={label} hint={hint}>
      <input
        type="text"
        className={mono ? "mono" : undefined}
        value={value}
        placeholder={placeholder}
        spellCheck={false}
        onChange={(e) => onChange(e.target.value)}
      />
    </Field>
  );
}

export function NumberField({ label, value, min, max, hint, onChange }: {
  label: string;
  value: number;
  min?: number;
  max?: number;
  hint?: string;
  onChange: (value: number) => void;
}) {
  return (
    <Field label={label} hint={hint}>
      <input
        type="number"
        value={value}
        min={min}
        max={max}
        onChange={(e) => {
          const n = e.target.valueAsNumber;
          if (!Number.isNaN(n)) onChange(n);
        }}
      />
    </Field>
  );
}

export function CheckField({ label, checked, hint, onChange }: {
  label: string;
  checked: boolean;
  hint?: string;
  onChange: (checked: boolean) => void;
}) {
  return (
    <div className="checkwrap">
      <label className="check">
        <input
          type="checkbox"
          checked={checked}
          onChange={(e) => onChange(e.target.checked)}
        />
        {label}
      </label>
      {hint && <p className="hint">{hint}</p>}
    </div>
  );
}
