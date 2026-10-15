import { definePlugin, type Action, type Element, type Navigation, type Slerm, type State, type TextColor } from "../src/index.js";

const ACTIONS: Action[] = [
  "split_right", "split_down", "new_floating", "toggle_floats", "zoom_pane", "new_tab", "next_tab", "close_pane",
];
// Keys sent while the pane table has focus. Escape returns to the terminal.
const MOVES: Record<string, Navigation> = { J: "next", K: "previous", O: "activate" };
const KEYS = [...Object.keys(MOVES), "R", "Q"];
let state: State | undefined;
let shown = false;
let lines = 20;
let preview = "";
let commands = "";
let submit = true;
let everywhere = false;
let action: Action = "split_right";
let status: { text: string; color?: TextColor } = { text: "" };

const panes = () => state?.tabs.flatMap((tab, index) => tab.panes.map(pane => ({ tab, index, pane }))) ?? [];
const activeTab = () => state?.tabs.find(tab => tab.id === state?.activeTab);
const active = () => panes().find(({ tab, pane }) => tab === activeTab() && pane.id === tab.activePane);

async function refreshPreview(api: Slerm) {
  const target = active();
  preview = target ? (await api.readTerminal({ pane: target.pane.id })).trimEnd().split("\n").slice(-lines).join("\n") : "";
}

async function show(api: Slerm) {
  if (!shown) return;
  const target = active();
  const elements: Element[] = [
    { type: "section", id: "panes-section", title: "Panes", elements: [
      { type: "table", id: "panes", columns: ["Tab", "Pane", "Title", "Directory"], selected: target ? String(target.pane.id) : undefined,
        rows: panes().map(({ index, pane }) => ({ id: String(pane.id), cells: [
          String(index + 1), String(pane.id) + (pane.floating ? " ◳" : ""), pane.exited ? "exited" : pane.title || "shell", pane.cwd ?? "",
        ] })) },
      { type: "text", text: "j/k focus · o zoom · r refresh preview · q close", style: "small" },
    ] },
    { type: "section", id: "preview-section", title: target ? `Preview · pane ${target.pane.id}` : "Preview", elements: [
      { type: "row", elements: [
        { type: "slider", id: "lines", label: "lines", value: lines, min: 5, max: 200, step: 5 },
        { type: "button", id: "refresh", label: "Refresh" },
      ] },
      { type: "textArea", id: "preview", value: preview, rows: 12, code: true, readonly: true, follow: true },
    ] },
    { type: "section", id: "send-section", title: "Send", open: false, elements: [
      { type: "textArea", id: "commands", value: commands, rows: 4, code: true },
      { type: "row", elements: [
        { type: "checkbox", id: "submit", label: "Press Enter", checked: submit },
        { type: "checkbox", id: "everywhere", label: "Every pane in tab", checked: everywhere },
      ] },
      { type: "button", id: "send", label: "Send" },
    ] },
    { type: "section", id: "actions-section", title: "Actions", open: false, elements: [
      { type: "row", elements: [
        { type: "select", id: "action", value: action, options: ACTIONS },
        { type: "button", id: "run", label: "Run" },
      ] },
    ] },
    { type: "separator" },
    { type: "row", elements: [{ type: "button", id: "close", label: "Close" }] },
  ];
  if (status.text) elements.push({ type: "text", text: status.text, color: status.color });
  await api.setPanel({ panel: { id: "panes", title: "Pane dashboard", placement: "right", keys: KEYS, elements } });
}

async function hide(api: Slerm) {
  shown = false;
  await api.removePanel({ panel: "panes" });
}

async function focus(api: Slerm, id: number) {
  const target = panes().find(({ pane }) => pane.id === id);
  if (!target) return;
  await api.focusTab({ tab: target.tab.id });
  await api.focusPane({ pane: id });
}

definePlugin({
  async activate(api) {
    await api.registerCommand({ command: { id: "toggle-panes", title: "Toggle pane dashboard" } });
    state = await api.getState();
  },
  async onEvent(event, api) {
    try {
      if (event.event === "command" && event.command === "toggle-panes") {
        if (shown) return await hide(api);
        shown = true;
        state = await api.getState();
        await refreshPreview(api);
        await show(api);
        await api.focusPanel({ panel: "panes", control: "panes" });
        return;
      } else if (event.event === "state") {
        const before = active()?.pane.id;
        state = event.state;
        if (active()?.pane.id !== before) await refreshPreview(api);
      } else if (event.event === "ui" && event.data.panel === "panes") {
        const data = event.data;
        status = { text: "" };
        switch (data.kind) {
          case "key": {
            const move = MOVES[data.key];
            if (move) return await api.navigate({ panel: "panes", control: "panes", to: move });
            if (data.key === "Q") return await hide(api);
            if (data.key === "R") await refreshPreview(api);
            break;
          }
          case "select":
            await focus(api, Number(data.item));
            return; // The state event that follows updates the panel.
          case "activate":
            await focus(api, Number(data.item));
            await api.execute({ action: "zoom_pane" });
            return;
          case "number":
            lines = data.value;
            await refreshPreview(api);
            break;
          case "check":
            if (data.control === "submit") submit = data.checked;
            if (data.control === "everywhere") everywhere = data.checked;
            return;
          case "change":
            if (data.control === "commands") commands = data.value;
            if (data.control === "action") action = data.value as Action;
            return;
          case "click":
            if (data.control === "close") return await hide(api);
            if (data.control === "refresh") await refreshPreview(api);
            if (data.control === "run") await api.execute({ action });
            if (data.control === "send") {
              const targets = everywhere ? activeTab()?.panes ?? [] : [active()?.pane].filter(p => p !== undefined);
              const text = commands.split("\n").join("\r") + (submit ? "\r" : "");
              for (const pane of targets) await api.writeTerminal({ pane: pane.id, text });
              status = { text: `Sent to ${targets.length} pane${targets.length === 1 ? "" : "s"}.`, color: "accent" };
            }
            break;
          default:
            return;
        }
      } else {
        return;
      }
      await show(api);
    } catch (e) {
      status = { text: String(e), color: "error" };
      await show(api);
    }
  },
});
