import { definePlugin, fs, path, Tree, type DirEntry, type Slerm, type State } from "../src/index.js";

// Use an absolute path if hx is not on the GUI application's PATH.
const EDITOR = "hx";
let root: string | undefined;
let follow = true;
let shown = true;
let error = "";
// The active pane and its directory when last followed, so manual navigation
// sticks until the user switches panes or changes directory.
let followed = "";

async function list(dir: string): Promise<DirEntry[]> {
  const entries = await fs.readDir({ path: dir });
  const folder = (entry: DirEntry) => entry.kind === "directory" ? 0 : 1;
  return entries.sort((a, b) => folder(a) - folder(b) || (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
}

const tree = new Tree<DirEntry>("tree", {
  roots: () => root ? list(root) : [],
  children: entry => list(entry.path),
  key: entry => entry.path,
  label: entry => entry.name + (entry.symlink ? " ↗" : ""),
  expandable: entry => entry.kind === "directory",
});

async function show(api: Slerm) {
  if (!shown) return;
  const files = tree.element();
  await api.setPanel({ panel: {
    id: "files", title: "Files", placement: "left", elements: [
      { type: "text", text: root ?? "No working directory available" },
      { type: "button", id: "up", label: "Parent directory" },
      { type: "button", id: "refresh", label: "Refresh" },
      { type: "button", id: "follow", label: follow ? "Following active pane — click to pin" : "Pinned — click to follow active pane" },
      { type: "button", id: "close", label: "Close sidebar" },
      { type: "text", text: error || (tree.truncated
        ? "Showing the first 8192 rows. Collapse folders to see more."
        : "Double-click or Enter to open. Arrow keys navigate.") },
      files,
    ],
  } });
}

async function open(dir: string | undefined) {
  root = dir;
  error = "";
  await tree.reset();
}

async function followState(state: State): Promise<boolean> {
  if (!follow || !shown) return false;
  const tab = state.tabs.find(tab => tab.id === state.activeTab);
  const pane = tab?.panes.find(pane => pane.id === tab.activePane);
  const location = `${pane?.id}:${pane?.cwd}`;
  if (location === followed) return false;
  followed = location;
  if (pane?.cwd !== root) await open(pane?.cwd ?? undefined);
  return true;
}

definePlugin({
  async activate(api) {
    await api.registerCommand({ command: { id: "show-files", title: "Show file tree" } });
    try { await followState(await api.getState()); }
    catch (e) { error = String(e); }
    await show(api);
  },
  async onEvent(event, api) {
    try {
      if (event.event === "command" && event.command === "show-files") {
        shown = true;
        followed = "";
        await followState(await api.getState());
      } else if (event.event === "state") {
        if (!await followState(event.state)) return;
      } else if (event.event === "ui" && event.data.panel === "files") {
        const data = event.data;
        if (data.kind === "click") {
          if (data.control === "close") { shown = false; await api.removePanel({ panel: "files" }); return; }
          if (data.control === "refresh") { error = ""; await tree.load(); }
          if (data.control === "up" && root && root !== "/") await open(path.dirname(root));
          if (data.control === "follow") {
            follow = !follow;
            followed = "";
            await followState(await api.getState());
          }
        } else {
          const action = await tree.handle(data);
          if (action?.kind === "select") return;
          if (action?.kind === "activate" && action.item.kind !== "directory" && root) {
            await api.openTerminal({ program: EDITOR, args: ["--", action.item.path], cwd: root });
          }
        }
      } else {
        return;
      }
      await show(api);
    } catch (e) {
      error = String(e);
      await show(api);
    }
  },
});
