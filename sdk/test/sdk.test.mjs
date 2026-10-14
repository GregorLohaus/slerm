import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext, runInContext } from "node:vm";
import test from "node:test";

const bridge = readFileSync(new URL("../runtime.js", import.meta.url), "utf8");
const example = readFileSync(new URL("../dist/workspace.js", import.meta.url), "utf8");
const settle = () => new Promise(resolve => setImmediate(resolve));

function start(reply = () => ({ result: null }), fs = () => ({ error: "no filesystem" })) {
  const operations = [];
  const errors = [];
  const context = {
    __slerm_version: 1,
    __slerm_call: text => {
      const operation = JSON.parse(text);
      operations.push(operation);
      return JSON.stringify(reply(operation));
    },
    __slerm_fs: text => JSON.stringify(fs(JSON.parse(text))),
    __slerm_error: error => errors.push(error),
  };
  runInNewContext(bridge, context);
  return { context, operations, errors };
}

test("bundled TypeScript example uses generated methods and handles UI and command events", async () => {
  const host = start(operation => ({ result: operation.method === "getState"
    ? { activeTab: 1, tabs: [{ id: 1, activePane: 2 }] }
    : operation.method === "readTerminal" ? "hello from terminal" : null }));
  runInContext(example, host.context);
  await settle();
  assert.deepEqual(host.operations.map(op => op.method), ["registerCommand", "setPanel"]);
  assert.equal(host.operations[1].panel.elements[3].type, "input");
  const ui = data => host.context.__slerm_dispatch({ event: "ui", data: { panel: "workspace", ...data } });
  ui({ kind: "change", control: "command", value: "echo hello" });
  await settle();
  ui({ kind: "click", control: "run" });
  await settle();
  assert.deepEqual(host.operations.at(-1), { method: "writeTerminal", pane: 2, text: "echo hello\r" });
  ui({ kind: "click", control: "scratch" });
  await settle();
  assert.deepEqual(host.operations.at(-1), { method: "execute", action: "new_floating" });
  ui({ kind: "click", control: "read" });
  await settle();
  assert.equal(host.operations.at(-1).panel.elements[0].text, "hello from terminal");
  ui({ kind: "click", control: "close" });
  await settle();
  assert.equal(host.operations.at(-1).method, "removePanel");
  host.context.__slerm_dispatch({ event: "command", command: "show-tools" });
  await settle();
  assert.equal(host.operations.at(-1).method, "setPanel");
  assert.deepEqual(host.errors, []);
});

test("API mismatch and duplicate registration fail before activation", () => {
  const { context } = start();
  assert.throws(() => context.__slerm.activate({}, 2), /Unsupported Slerm API/);
  context.__slerm.activate({}, 1);
  assert.throws(() => context.__slerm.activate({}, 1), /exactly one plugin/);
});

test("host errors reject generated calls and surface callback failures", async () => {
  const host = start(() => ({ error: "unknown pane" }));
  runInContext(example, host.context);
  await settle();
  assert.deepEqual(host.errors, ["Error: unknown pane"]);
});

test("file tree follows the active pane, loads folders lazily, and opens files", async () => {
  const files = {
    "/project": ["src", "README.md"],
    "/project/src": ["main.rs", "lib"],
    "/project/src/lib": ["mod.rs"],
    "/other": ["notes.txt"],
  };
  const reads = [];
  const fs = op => {
    assert.equal(op.method, "readDir");
    reads.push(op.path);
    if (!files[op.path]) return { error: `Cannot read ${op.path}` };
    return { result: files[op.path].map(name => {
      const path = `${op.path}/${name}`;
      return { name, path, kind: files[path] ? "directory" : "file", symlink: false };
    }).sort((a, b) => a.name < b.name ? -1 : 1) };
  };
  let cwd = "/project";
  const state = () => ({ activeTab: 1, tabs: [{ id: 1, activePane: 2, panes: [{ id: 2, cwd }] }] });
  const host = start(op => ({ result: op.method === "getState" ? state() : null }), fs);
  runInContext(readFileSync(new URL("../dist/filetree.js", import.meta.url), "utf8"), host.context);
  await settle();
  const panel = () => host.operations.findLast(op => op.method === "setPanel").panel;
  const tree = () => panel().elements.find(e => e.type === "tree");
  const row = label => tree().rows.find(row => row.label === label);
  const ui = data => { host.context.__slerm_dispatch({ event: "ui", data: { panel: "files", control: "tree", ...data } }); return settle(); };

  assert.equal(panel().placement, "left");
  assert.deepEqual(tree().rows.map(r => [r.label, r.depth, r.expandable]), [["src", 0, true], ["README.md", 0, false]]);
  assert.deepEqual(reads, ["/project"]);

  await ui({ kind: "toggle", item: row("src").id, expanded: true });
  assert.deepEqual(tree().rows.map(r => r.label), ["src", "lib", "main.rs", "README.md"]);
  await ui({ kind: "toggle", item: row("lib").id, expanded: true });
  assert.equal(row("mod.rs").depth, 2);
  assert.ok(row("src").id.length < 4, "row ids stay compact");

  // Collapsing and reopening a parent restores expanded descendants.
  await ui({ kind: "toggle", item: row("src").id, expanded: false });
  assert.equal(row("mod.rs"), undefined);
  await ui({ kind: "toggle", item: row("src").id, expanded: true });
  assert.ok(row("mod.rs"));

  const updates = host.operations.length;
  await ui({ kind: "select", item: row("main.rs").id });
  assert.equal(host.operations.length, updates, "selection needs no panel update");
  await ui({ kind: "activate", item: row("main.rs").id });
  assert.deepEqual(host.operations.at(-2), { method: "openTerminal", program: "hx", args: ["--", "/project/src/main.rs"], cwd: "/project" });
  assert.equal(tree().selected, row("main.rs").id);

  // Refresh keeps expansion; a folder that vanished collapses.
  delete files["/project/src/lib"];
  files["/project/src"] = ["main.rs"];
  host.context.__slerm_dispatch({ event: "ui", data: { panel: "files", kind: "click", control: "refresh" } });
  await settle();
  assert.deepEqual(tree().rows.map(r => r.label), ["src", "main.rs", "README.md"]);

  // Unreadable folders report an error and stay collapsed.
  files["/project/src"] = ["main.rs", "locked"];
  files["/project/src/locked"] = [];
  host.context.__slerm_dispatch({ event: "ui", data: { panel: "files", kind: "click", control: "refresh" } });
  await settle();
  delete files["/project/src/locked"];
  await ui({ kind: "toggle", item: row("locked").id, expanded: true });
  assert.match(panel().elements[5].text, /Cannot read \/project\/src\/locked/);
  assert.equal(row("locked").expanded, false);

  host.context.__slerm_dispatch({ event: "ui", data: { panel: "files", kind: "click", control: "up" } });
  await settle();
  assert.equal(panel().elements[0].text, "/");

  cwd = "/other";
  host.context.__slerm_dispatch({ event: "state", state: state() });
  await settle();
  assert.equal(panel().elements[0].text, "/other");
  assert.deepEqual(tree().rows.map(r => r.label), ["notes.txt"]);
  assert.deepEqual(host.errors, []);
});
