/**
 * Dictionary terms and snippet expansions.
 *
 * The dictionary biases STT toward words it would otherwise mishear; snippets
 * are exact trigger → expansion pairs expanded in Rust before any cleanup
 * model runs (PROJECT_NOTES §13 Task 3), so the UI keeps them as dumb pairs —
 * no syntax, no preview, nothing to get out of sync with the expander.
 */

import { useState } from "react";

import type { Settings } from "../lib/ipc";

export default function DictionaryTab({
  settings,
  onChange,
}: {
  settings: Settings;
  onChange: (next: Settings) => void;
}) {
  const [term, setTerm] = useState("");
  const [trigger, setTrigger] = useState("");
  const [expansion, setExpansion] = useState("");

  const addTerm = () => {
    const t = term.trim();
    if (t === "" || settings.dictionary.includes(t)) return;
    onChange({ ...settings, dictionary: [...settings.dictionary, t] });
    setTerm("");
  };

  const addSnippet = () => {
    const trig = trigger.trim();
    const exp = expansion.trim();
    if (trig === "" || exp === "") return;
    if (settings.snippets.some((s) => s.trigger === trig)) return;
    onChange({
      ...settings,
      snippets: [...settings.snippets, { trigger: trig, expansion: exp }],
    });
    setTrigger("");
    setExpansion("");
  };

  return (
    <>
      <section className="card">
        <h2>Dictionary</h2>
        <p className="hint">Terms the STT pass is biased toward — names, jargon, product words.</p>
        {settings.dictionary.length === 0 ? (
          <p className="hint">No terms yet.</p>
        ) : (
          <ul className="editlist">
            {settings.dictionary.map((t) => (
              <li key={t}>
                <span className="selectable">{t}</span>
                <button
                  className="link"
                  onClick={() =>
                    onChange({
                      ...settings,
                      dictionary: settings.dictionary.filter((x) => x !== t),
                    })
                  }
                >
                  remove
                </button>
              </li>
            ))}
          </ul>
        )}
        <form
          className="addrow"
          onSubmit={(e) => {
            e.preventDefault();
            addTerm();
          }}
        >
          <input
            type="text"
            value={term}
            placeholder="Add a term…"
            onChange={(e) => setTerm(e.target.value)}
          />
          <button type="submit" disabled={term.trim() === ""}>
            Add
          </button>
        </form>
      </section>

      <section className="card">
        <h2>Snippets</h2>
        <p className="hint">
          Saying the trigger expands to the full text, exactly, before cleanup runs.
        </p>
        {settings.snippets.length === 0 ? (
          <p className="hint">No snippets yet.</p>
        ) : (
          <ul className="editlist">
            {settings.snippets.map((s) => (
              <li key={s.trigger}>
                <span className="selectable">
                  <kbd>{s.trigger}</kbd> → {s.expansion}
                </span>
                <button
                  className="link"
                  onClick={() =>
                    onChange({
                      ...settings,
                      snippets: settings.snippets.filter((x) => x.trigger !== s.trigger),
                    })
                  }
                >
                  remove
                </button>
              </li>
            ))}
          </ul>
        )}
        <form
          className="addrow"
          onSubmit={(e) => {
            e.preventDefault();
            addSnippet();
          }}
        >
          <input
            type="text"
            value={trigger}
            placeholder="trigger"
            onChange={(e) => setTrigger(e.target.value)}
          />
          <input
            type="text"
            value={expansion}
            placeholder="expansion"
            onChange={(e) => setExpansion(e.target.value)}
          />
          <button type="submit" disabled={trigger.trim() === "" || expansion.trim() === ""}>
            Add
          </button>
        </form>
      </section>
    </>
  );
}
