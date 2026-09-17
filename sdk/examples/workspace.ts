import { definePlugin, type Slerm } from "../src/index.js";

let command = "pwd";
const show = (slerm: Slerm) => slerm.setPanel({ panel: {
  id: "workspace", title: "Workspace tools", elements: [
    { type: "text", text: "Create a scratch terminal or run a command in the active pane." },
    { type: "button", id: "scratch", label: "New floating pane" },
    { type: "separator" },
    { type: "input", id: "command", label: "Shell command", value: command },
    { type: "button", id: "run", label: "Run in active pane" },
    { type: "button", id: "read", label: "Show terminal text" },
    { type: "button", id: "close", label: "Close tools" },
  ],
} });

definePlugin({
  async activate(slerm) {
    await slerm.registerCommand({ command: { id: "show-tools", title: "Show workspace tools" } });
    await show(slerm);
  },
  async onEvent(event, slerm) {
    if (event.event === "command" && event.command === "show-tools") await show(slerm);
    if (event.event !== "ui" || event.data.panel !== "workspace") return;
    const control = event.data;
    if (control.kind === "change" && control.control === "command") command = control.value;
    if (control.kind !== "click") return;
    if (control.control === "scratch") await slerm.execute({ action: "new_floating" });
    if (control.control === "close") await slerm.removePanel({ panel: "workspace" });
    if (control.control === "run" || control.control === "read") {
      const state = await slerm.getState();
      const pane = state.tabs.find(tab => tab.id === state.activeTab)?.activePane;
      if (pane === undefined) return;
      if (control.control === "run") await slerm.writeTerminal({ pane, text: command + "\r" });
      else await slerm.setPanel({ panel: {
        id: "output", title: "Terminal snapshot",
        elements: [{ type: "text", text: await slerm.readTerminal({ pane }) }],
      } });
    }
  },
});
