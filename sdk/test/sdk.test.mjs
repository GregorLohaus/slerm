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
  const text = () => panel().elements.filter(e => e.type === "text").map(e => e.text);
  // Render loaded nodes as an indented outline; "+" marks an unloaded branch.
  const outline = (nodes = panel().elements.at(-1).nodes, depth = 0) => nodes.flatMap(node => [
    "  ".repeat(depth) + node.label + (node.branch && !node.children ? "+" : ""),
    ...outline(node.children ?? [], depth + 1),
  ]);
  const ui = data => { host.context.__slerm_dispatch({ event: "ui", data: { panel: "files", control: "tree", ...data } }); return settle(); };
  const click = control => ui({ kind: "click", control });

  assert.equal(panel().placement, "left");
  assert.deepEqual(outline(), ["src+", "README.md"]);
  assert.deepEqual(reads, ["/project"]);

  await ui({ kind: "toggle", item: "/project/src", expanded: true });
  await ui({ kind: "toggle", item: "/project/src/lib", expanded: true });
  assert.deepEqual(outline(), ["src", "  lib", "    mod.rs", "  main.rs", "README.md"]);

  // Collapsing and reopening needs no reload or panel update: the host owns expansion.
  const updates = host.operations.length;
  await ui({ kind: "toggle", item: "/project/src", expanded: false });
  await ui({ kind: "toggle", item: "/project/src", expanded: true });
  await ui({ kind: "select", item: "/project/src/main.rs" });
  assert.equal(host.operations.length, updates);
  assert.deepEqual(reads, ["/project", "/project/src", "/project/src/lib"]);

  await ui({ kind: "activate", item: "/project/src/main.rs" });
  assert.deepEqual(host.operations.at(-1), { method: "openTerminal", program: "hx", args: ["--", "/project/src/main.rs"], cwd: "/project" });

  // Refresh reloads loaded folders; one that vanished drops out.
  delete files["/project/src/lib"];
  files["/project/src"] = ["main.rs"];
  await click("refresh");
  assert.deepEqual(outline(), ["src", "  main.rs", "README.md"]);

  // Unreadable folders report an error.
  files["/project/src"] = ["main.rs", "locked"];
  files["/project/src/locked"] = [];
  await click("refresh");
  delete files["/project/src/locked"];
  await ui({ kind: "toggle", item: "/project/src/locked", expanded: true });
  assert.match(text().at(-1), /Cannot read \/project\/src\/locked/);
  assert.ok(outline().includes("  locked+"));

  await click("up");
  assert.equal(text()[0], "/");
  assert.deepEqual(panel().elements[0].elements.map(e => e.id), ["up", "refresh", "follow", "close"]);

  cwd = "/other";
  host.context.__slerm_dispatch({ event: "state", state: state() });
  await settle();
  assert.equal(text()[0], "/other");
  assert.deepEqual(outline(), ["notes.txt"]);
  assert.deepEqual(host.errors, []);
});
