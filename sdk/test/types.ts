import type { Slerm, State } from "../src/index.js";

declare const api: Slerm;
const state: Promise<State> = api.getState();
const text: Promise<string> = api.readTerminal({ pane: 1 });
void state; void text;
// @ts-expect-error Generated methods reject unknown actions.
api.execute({ action: "missing" });
// @ts-expect-error Pane IDs are numbers in the JSON API.
api.focusPane({ pane: "1" });
// @ts-expect-error A terminal write requires text.
api.writeTerminal({ pane: 1 });
// @ts-expect-error Parameterless methods take no parameters.
api.getState({});
// @ts-expect-error Panel elements are generated from the Rust enum.
api.setPanel({ panel: { id: "test", title: "Test", elements: [{ type: "html", html: "" }] } });
// @ts-expect-error Return types come from the Rust method list.
const invalid: Promise<number> = api.readTerminal({ pane: 1 });
void invalid;
