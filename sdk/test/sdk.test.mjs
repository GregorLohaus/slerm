import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext, runInContext } from "node:vm";
import test from "node:test";

const bridge = readFileSync(new URL("../runtime.js", import.meta.url), "utf8");
const example = readFileSync(new URL("../dist/panes.js", import.meta.url), "utf8");
const settle = () => new Promise(resolve => setImmediate(resolve));

function start(reply = () => ({ result: null }), fs = () => ({ error: "no filesystem" }), system = () => ({ error: "no system access" })) {
  const operations = [];
  const errors = [];
  const timers = new Map();
  const context = {
    __slerm_version: 1,
    // Record operations flat, as { method, ...params }, to keep assertions short.
    __slerm_call: text => {
      const { method, params } = JSON.parse(text);
      const operation = { method, ...params };
      operations.push(operation);
      return JSON.stringify(reply(operation));
    },
    __slerm_fs: text => {
      const { method, params } = JSON.parse(text);
      return JSON.stringify(fs({ method, ...params }));
    },
    __slerm_system: text => {
      const { method, params } = JSON.parse(text);
      return JSON.stringify(system({ method, ...params }));
    },
    __slerm_timer: (handle, ms) => timers.set(handle, ms),
    __slerm_error: error => errors.push(error),
  };
  runInNewContext(bridge, context);
  return { context, operations, errors, timers };
}

test("pane dashboard drives panes through tables, sliders, checkboxes, and selects", async () => {
  let state = { activeTab: 1, tabs: [
    { id: 1, activePane: 2, panes: [{ id: 2, title: "zsh", cwd: "/a", floating: false, exited: false }, { id: 3, title: "", cwd: null, floating: true, exited: false }] },
    { id: 4, activePane: 5, panes: [{ id: 5, title: "hx", cwd: "/b", floating: false, exited: false }] },
  ] };
  const output = Array.from({ length: 50 }, (_, i) => `line ${i + 1}`).join("\n") + "\n\n";
  const host = start(op => ({ result: op.method === "getState" ? state : op.method === "readTerminal" ? output : null }));
  runInContext(example, host.context);
  await settle();
  assert.ok(!host.operations.some(op => op.method === "setPanel"), "starts hidden");
  host.context.__slerm_dispatch({ event: "command", command: "toggle-panes" });
  await settle();
  const panel = () => host.operations.findLast(op => op.method === "setPanel").panel;
  const all = elements => elements.flatMap(e => [e, ...all(e.elements ?? [])]);
  const find = id => all(panel().elements).find(e => e.id === id);
  const ui = data => { host.context.__slerm_dispatch({ event: "ui", data: { panel: "panes", ...data } }); return settle(); };

  assert.equal(panel().placement, "right");
  assert.deepEqual(find("panes").rows.map(row => row.cells), [["1", "2", "zsh", "/a"], ["1", "3 ◳", "shell", ""], ["2", "5", "hx", "/b"]]);
  assert.equal(find("panes").selected, "2");
  assert.equal(find("preview").value.split("\n").length, 20);
  assert.equal(find("preview").value.split("\n").at(-1), "line 50");

  await ui({ kind: "select", control: "panes", item: "5" });
  assert.deepEqual(host.operations.slice(-2), [{ method: "focusTab", tab: 4 }, { method: "focusPane", pane: 5 }]);
  state = { ...state, activeTab: 4 };
  host.context.__slerm_dispatch({ event: "state", state });
  await settle();
  assert.equal(find("panes").selected, "5");
  assert.equal(host.operations.findLast(op => op.method === "readTerminal").pane, 5);

  await ui({ kind: "number", control: "lines", value: 5 });
  assert.equal(find("preview").value, "line 46\nline 47\nline 48\nline 49\nline 50");
  assert.equal(find("lines").value, 5);

  await ui({ kind: "change", control: "commands", value: "ls\npwd" });
  await ui({ kind: "check", control: "submit", checked: false });
  await ui({ kind: "click", control: "send" });
  assert.deepEqual(host.operations.findLast(op => op.method === "writeTerminal"), { method: "writeTerminal", pane: 5, text: "ls\rpwd" });
  assert.match(panel().elements.at(-1).text, /Sent to 1 pane/);
  state = { ...state, activeTab: 1 };
  host.context.__slerm_dispatch({ event: "state", state });
  await settle();
  await ui({ kind: "check", control: "everywhere", checked: true });
  await ui({ kind: "click", control: "send" });
  assert.deepEqual(host.operations.filter(op => op.method === "writeTerminal").slice(-2).map(op => op.pane), [2, 3]);

  await ui({ kind: "change", control: "action", value: "new_tab" });
  await ui({ kind: "click", control: "run" });
  assert.deepEqual(host.operations.at(-2), { method: "execute", action: "new_tab" });
  await ui({ kind: "activate", control: "panes", item: "3" });
  assert.deepEqual(host.operations.at(-1), { method: "execute", action: "zoom_pane" });

  await ui({ kind: "click", control: "close" });
  assert.equal(host.operations.at(-1).method, "removePanel");
  host.context.__slerm_dispatch({ event: "command", command: "toggle-panes" });
  await settle();
  assert.deepEqual(host.operations.slice(-2).map(op => op.method), ["setPanel", "focusPanel"]);
  assert.deepEqual(host.operations.at(-1), { method: "focusPanel", panel: "panes", control: "panes" });
  await ui({ kind: "key", control: "panes", key: "J" });
  assert.deepEqual(host.operations.at(-1), { method: "navigate", panel: "panes", control: "panes", to: "next" });
  await ui({ kind: "key", control: "panes", key: "Q" });
  assert.equal(host.operations.at(-1).method, "removePanel");
  host.context.__slerm_dispatch({ event: "command", command: "toggle-panes" });
  await settle();
  host.context.__slerm_dispatch({ event: "command", command: "toggle-panes" });
  await settle();
  assert.equal(host.operations.at(-1).method, "removePanel");
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
  assert.deepEqual(host.operations.map(op => op.method), ["registerCommand"], "starts hidden");
  host.context.__slerm_dispatch({ event: "command", command: "toggle-files" });
  await settle();
  assert.deepEqual(host.operations.at(-1), { method: "focusPanel", panel: "files", control: "tree" });
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
  assert.ok(text().some(line => /Cannot read \/project\/src\/locked/.test(line)));
  assert.ok(outline().includes("  locked+"));

  await click("up");
  assert.equal(text()[0], "/");
  assert.deepEqual(panel().elements[0].elements.map(e => e.id), ["up", "refresh", "follow", "close"]);

  cwd = "/other";
  host.context.__slerm_dispatch({ event: "state", state: state() });
  await settle();
  assert.equal(text()[0], "/other");
  assert.deepEqual(outline(), ["notes.txt"]);
  // Vim keys drive the native tree; gg needs two presses.
  const key = key => ui({ kind: "key", key });
  await key("J");
  assert.deepEqual(host.operations.at(-1), { method: "navigate", panel: "files", control: "tree", to: "next" });
  await key("Shift+G");
  assert.equal(host.operations.at(-1).to, "last");
  const before = host.operations.length;
  await key("G");
  assert.equal(host.operations.length, before);
  await key("G");
  assert.equal(host.operations.at(-1).to, "first");
  assert.deepEqual(panel().keys.sort(), ["Ctrl+D", "Ctrl+U", "G", "H", "J", "K", "L", "Minus", "O", "Q", "R", "Shift+G"]);
  await key("Minus");
  assert.equal(text()[0], "/");
  await key("Q");
  assert.equal(host.operations.at(-1).method, "removePanel");
  host.context.__slerm_dispatch({ event: "command", command: "toggle-files" });
  await settle();
  host.context.__slerm_dispatch({ event: "command", command: "toggle-files" });
  await settle();
  assert.equal(host.operations.at(-1).method, "removePanel");
  assert.deepEqual(host.errors, []);
});

test("git panel lists changes, stages files, commits, and refreshes on a timer", async () => {
  let porcelain = " M src/app.rs\0?? notes.md\0R  new.rs\0old.rs\0";
  const runs = [];
  const system = op => {
    assert.equal(op.method, "run");
    assert.equal(op.program, "git");
    runs.push(op.args.join(" "));
    const [command] = op.args;
    const stdout = command === "rev-parse" ? (op.cwd.startsWith("/repo") ? "/repo\n" : null)
      : command === "branch" ? "main\n"
      : command === "status" ? porcelain
      : command === "commit" ? "[main abc123] Ship it\n 1 file changed\n" : "";
    if (stdout === null) return { result: { status: 128, stdout: "", stderr: "fatal: not a git repository", timedOut: false } };
    return { result: { status: 0, stdout, stderr: "", timedOut: false } };
  };
  let cwd = "/repo/src";
  const state = () => ({ activeTab: 1, tabs: [{ id: 1, activePane: 2, panes: [{ id: 2, cwd }] }] });
  const host = start(op => ({ result: op.method === "getState" ? state() : null }), undefined, system);
  runInContext(readFileSync(new URL("../dist/git.js", import.meta.url), "utf8"), host.context);
  await settle();
  assert.ok(!runs.length, "a hidden panel runs nothing");
  host.context.__slerm_dispatch({ event: "command", command: "toggle-git" });
  await settle();
  const panel = () => host.operations.findLast(op => op.method === "setPanel").panel;
  const all = elements => elements.flatMap(e => [e, ...all(e.elements ?? [])]);
  const find = id => all(panel().elements).find(e => e.id === id);
  const texts = () => all(panel().elements).filter(e => e.type === "text").map(e => e.text);
  const ui = data => { host.context.__slerm_dispatch({ event: "ui", data: { panel: "git", ...data } }); return settle(); };

  assert.ok(texts().includes("\ue0a0 main"));
  assert.deepEqual(find("changes").rows.map(row => row.cells), [
    ["modified", "src/app.rs"], ["new", "notes.md"], ["renamed (staged)", "new.rs"],
  ]);

  assert.deepEqual(host.operations.at(-1), { method: "focusPanel", panel: "git", control: "changes" });
  await ui({ kind: "key", control: "changes", key: "J" });
  assert.deepEqual(host.operations.at(-1), { method: "navigate", panel: "git", control: "changes", to: "next" });
  await ui({ kind: "select", control: "changes", item: "src/app.rs" });
  await ui({ kind: "key", control: "changes", key: "S" });
  assert.ok(runs.includes("add -- src/app.rs"));
  await ui({ kind: "key", control: "changes", key: "C" });
  assert.deepEqual(host.operations.at(-1), { method: "focusPanel", panel: "git", control: "message" });
  await ui({ kind: "select", control: "changes", item: "notes.md" });
  await ui({ kind: "click", control: "stage" });
  assert.ok(runs.includes("add -- notes.md"));
  await ui({ kind: "click", control: "commit" });
  assert.ok(texts().includes("Write a commit message first."));
  await ui({ kind: "change", control: "message", value: "Ship it" });
  await ui({ kind: "click", control: "commit" });
  assert.ok(runs.includes("commit -m Ship it"));
  assert.ok(texts().includes("[main abc123] Ship it"));
  assert.equal(find("message").value, "");

  // The interval refreshes, and only changed output updates the panel.
  const [[handle, ms]] = host.timers;
  assert.equal(ms, 2000);
  const updates = host.operations.filter(op => op.method === "setPanel").length;
  host.context.__slerm_fire(handle);
  await settle();
  assert.equal(host.operations.filter(op => op.method === "setPanel").length, updates);
  porcelain = "";
  host.context.__slerm_fire(handle);
  await settle();
  assert.ok(texts().includes("Working tree clean"));

  await ui({ kind: "activate", control: "changes", item: "src/app.rs" });
  cwd = "/elsewhere";
  host.context.__slerm_dispatch({ event: "state", state: state() });
  await settle();
  assert.ok(texts().includes("Not a git repository"));
  await ui({ kind: "click", control: "close" });
  assert.equal(host.operations.at(-1).method, "removePanel");
  assert.equal(host.timers.size, 1, "closing clears the interval without new timers");
  assert.deepEqual(host.errors, []);
});
