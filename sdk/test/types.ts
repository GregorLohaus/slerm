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

import { fs, type DirEntry } from "../src/index.js";
const entries: Promise<DirEntry[]> = fs.readDir({ path: "/" });
void entries;
// @ts-expect-error Filesystem calls take named parameters.
fs.readFile("/etc/hosts");
api.setPanel({ panel: { id: "files", title: "Files", placement: "left", elements: [
  { type: "row", elements: [{ type: "button", id: "up", label: "Up" }] },
  { type: "tree", id: "tree", nodes: [{ id: "a", label: "a", icon: "x", branch: true, children: [{ id: "b", label: "b" }] }] },
] } });
// @ts-expect-error Tree nodes are generated from the Rust struct.
api.setPanel({ panel: { id: "files", title: "Files", elements: [{ type: "tree", id: "tree", nodes: [{ id: "a", label: "a", depth: 0 }] }] } });

import { system, type RunOutput, type FetchResponse } from "../src/index.js";
const ran: Promise<RunOutput> = system.run({ program: "git", args: ["status"], cwd: "/" });
const fetched: Promise<FetchResponse> = system.fetch({ url: "https://example.com", method: "POST", headers: { accept: "text/plain" }, body: "" });
const home: Promise<string | null> = system.env({ name: "HOME" });
void ran; void fetched; void home;
// @ts-expect-error run needs a working directory.
system.run({ program: "git" });
const handle: number = setTimeout((a: string) => void a, 10, "x");
clearTimeout(handle);
// @ts-expect-error Timer arguments match the callback.
setTimeout((a: string) => void a, 10, 1);
