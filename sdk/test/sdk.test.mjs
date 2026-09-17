import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext, runInContext } from "node:vm";
import test from "node:test";

const bridge = readFileSync(new URL("../runtime.js", import.meta.url), "utf8");
const example = readFileSync(new URL("../dist/workspace.js", import.meta.url), "utf8");
const settle = () => new Promise(resolve => setImmediate(resolve));

function start(reply = () => ({ result: null })) {
  const operations = [];
  const errors = [];
  const context = {
    __slerm_version: 1,
    __slerm_call: text => {
      const operation = JSON.parse(text);
      operations.push(operation);
      return JSON.stringify(reply(operation));
    },
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
