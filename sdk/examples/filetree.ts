import { definePlugin, fs, type DirEntry, type Element, type Slerm, type State, type TreeNode } from "../src/index.js";

// Use an absolute path if hx is not on the GUI application's PATH.
const EDITOR = "hx";
// Nerd Font glyphs. Slerm's UI font falls back to the terminal's fonts.
const FOLDER = "";
const FILE = "";
const ICONS: Record<string, string> = {
  rs: "", ts: "", js: "", mjs: "", json: "", md: "",
  toml: "", nix: "", lock: "", sh: "", svg: "", png: "",
};
// The host accepts at most 16384 tree nodes per panel.
const MAX_NODES = 16_000;

let root: string | undefined;
// Listings of the directories loaded so far. The host keeps expansion state, so
// collapsed folders stay loaded and reopen instantly.
const listings = new Map<string, DirEntry[]>();
let follow = true;
let shown = true;
let error = "";
// The active pane and its directory when last followed, so manual navigation
// sticks until the user switches panes or changes directory.
let followed = "";

async function load(dir: string) {
  const folder = (entry: DirEntry) => entry.kind === "directory" ? 0 : 1;
  const entries = await fs.readDir({ path: dir });
  listings.set(dir, entries.sort((a, b) => folder(a) - folder(b) || (a.name < b.name ? -1 : a.name > b.name ? 1 : 0)));
}

async function open(dir: string | undefined) {
  root = dir;
  error = "";
  listings.clear();
  if (dir) await load(dir);
}

async function refresh() {
  error = "";
  for (const dir of [...listings.keys()]) {
    // A folder that vanished drops out with its listing.
    try { await load(dir); } catch { listings.delete(dir); }
  }
}

function nodes(dir: string, budget: { left: number }): TreeNode[] {
  const result: TreeNode[] = [];
  for (const entry of listings.get(dir) ?? []) {
    if (budget.left-- <= 0) break;
    const label = entry.symlink ? `${entry.name} ↗` : entry.name;
    if (entry.kind !== "directory") {
      result.push({ id: entry.path, label, icon: ICONS[entry.name.split(".").pop()!] ?? FILE });
    } else {
      // Unloaded folders are branches without children; expanding one asks us to load it.
      result.push({ id: entry.path, label, icon: FOLDER, branch: true,
        children: listings.has(entry.path) ? nodes(entry.path, budget) : undefined });
    }
  }
  return result;
}

async function show(api: Slerm) {
  if (!shown) return;
  const budget = { left: MAX_NODES };
  const tree = root ? nodes(root, budget) : [];
  const status = error || (budget.left < 0 ? "Too many files to show. Collapse or refresh folders." : "");
  const elements: Element[] = [
    { type: "row", elements: [
      { type: "button", id: "up", label: "Up" },
      { type: "button", id: "refresh", label: "Refresh" },
      { type: "button", id: "follow", label: follow ? "Pin" : "Follow" },
      { type: "button", id: "close", label: "Close" },
    ] },
    { type: "text", text: root ?? "No working directory" },
  ];
  if (status) elements.push({ type: "text", text: status });
  elements.push({ type: "tree", id: "tree", nodes: tree });
  await api.setPanel({ panel: { id: "files", title: "Files", placement: "left", elements } });
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
          if (data.control === "refresh") await refresh();
          if (data.control === "up" && root && root !== "/") await open(root.slice(0, root.lastIndexOf("/")) || "/");
          if (data.control === "follow") {
            follow = !follow;
            followed = "";
            await followState(await api.getState());
          }
        } else if (data.kind === "toggle") {
          if (!data.expanded || listings.has(data.item)) return;
          await load(data.item);
        } else if (data.kind === "activate" && root) {
          await api.openTerminal({ program: EDITOR, args: ["--", data.item], cwd: root });
          return;
        } else {
          return;
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
