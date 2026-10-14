import { definePlugin, system, type Element, type Navigation, type Slerm, type State, type TextColor } from "../src/index.js";

// Use an absolute path if git is not on the GUI application's PATH.
const GIT = "git";
const REFRESH_MS = 2000;
// Keys sent while the file list has focus. Escape returns to the terminal.
const MOVES: Record<string, Navigation> = { J: "next", K: "previous", O: "activate", "Shift+G": "last" };
const ACTIONS: Record<string, string> = { S: "stage", U: "unstage", A: "stage-all", R: "refresh", Q: "close" };
const KEYS = [...Object.keys(MOVES), ...Object.keys(ACTIONS), "C"];

type Change = { status: string; path: string };
let cwd: string | undefined;
let root: string | undefined;
let branch = "";
let changes: Change[] = [];
let selected: string | undefined;
let message = "";
let status: { text: string; color?: TextColor } = { text: "" };
let shown = false;
let busy = false;
let rendered = "";
let timer: number | undefined;

async function git(args: string[], dir = root ?? cwd) {
  if (!dir) throw new Error("No working directory");
  const out = await system.run({ program: GIT, args, cwd: dir });
  if (out.status !== 0) throw new Error((out.stderr || out.stdout).trim() || `git ${args[0]} failed`);
  return out.stdout;
}

// `git status -z` separates entries with NUL; renames carry the old path next.
function parse(text: string): Change[] {
  const fields = text.split("\0");
  const result: Change[] = [];
  for (let i = 0; i < fields.length; i++) {
    const field = fields[i];
    if (field.length < 4) continue;
    const status = field.slice(0, 2);
    result.push({ status, path: field.slice(3) });
    if (status.includes("R") || status.includes("C")) i++;
  }
  return result;
}

async function refresh() {
  if (!cwd) { root = undefined; changes = []; return; }
  try {
    root = (await git(["rev-parse", "--show-toplevel"], cwd)).trim();
    branch = (await git(["branch", "--show-current"])).trim() || "(detached)";
    changes = parse(await git(["status", "--porcelain=v1", "-z", "--untracked-files=all"]));
  } catch {
    root = undefined;
    changes = [];
  }
  if (selected && !changes.some(change => change.path === selected)) selected = undefined;
}

const describe = (status: string) => {
  if (status === "??") return "new";
  const names: Record<string, string> = { M: "modified", A: "added", D: "deleted", R: "renamed", C: "copied", U: "conflict" };
  const staged = status[0] !== " " ? names[status[0]] : undefined;
  const unstaged = status[1] !== " " ? names[status[1]] : undefined;
  return [staged && `${staged} (staged)`, unstaged].filter(Boolean).join(", ");
};

async function show(api: Slerm) {
  if (!shown) return;
  const elements: Element[] = root ? [
    { type: "text", text: ` ${branch}`, style: "heading" },
    { type: "text", text: root, style: "small" },
    { type: "section", id: "changes-section", title: `Changes (${changes.length})`, elements: [
      changes.length
        ? { type: "table", id: "changes", columns: ["Status", "File"], selected,
            rows: changes.map(change => ({ id: change.path, cells: [describe(change.status), change.path] })) }
        : { type: "text", text: "Working tree clean", style: "weak" },
      { type: "row", elements: [
        { type: "button", id: "stage", label: "Stage" },
        { type: "button", id: "unstage", label: "Unstage" },
        { type: "button", id: "stage-all", label: "Stage all" },
      ] },
      { type: "text", text: "j/k move · o diff · s stage · u unstage · a all · c commit · q close", style: "small" },
    ] },
    { type: "section", id: "commit-section", title: "Commit", elements: [
      { type: "textArea", id: "message", value: message, rows: 3 },
      { type: "row", elements: [
        { type: "button", id: "commit", label: "Commit" },
        ...(busy ? [{ type: "spinner" } as const] : []),
      ] },
    ] },
  ] : [
    { type: "text", text: "Not a git repository", style: "heading" },
    { type: "text", text: cwd ?? "No working directory", style: "weak" },
  ];
  if (status.text) elements.push({ type: "text", text: status.text, color: status.color });
  elements.push({ type: "separator" }, { type: "row", elements: [{ type: "button", id: "close", label: "Close" }] });
  const panel = { id: "git", title: "Git", placement: "right" as const, keys: KEYS, elements };
  // The timer refreshes often; skip identical updates.
  const text = JSON.stringify(panel);
  if (text === rendered) return;
  rendered = text;
  await api.setPanel({ panel });
}

function follow(state: State) {
  const tab = state.tabs.find(tab => tab.id === state.activeTab);
  return tab?.panes.find(pane => pane.id === tab.activePane)?.cwd ?? undefined;
}

async function open(api: Slerm) {
  shown = true;
  rendered = "";
  clearInterval(timer);
  timer = setInterval(async () => {
    if (busy) return;
    await refresh();
    await show(api);
  }, REFRESH_MS);
  await refresh();
  await show(api);
  if (changes.length) await api.focusPanel({ panel: "git", control: "changes" });
}

async function close(api: Slerm) {
  shown = false;
  clearInterval(timer);
  await api.removePanel({ panel: "git" });
}

definePlugin({
  async activate(api) {
    await api.registerCommand({ command: { id: "toggle-git", title: "Toggle git panel" } });
    cwd = follow(await api.getState());
  },
  async onEvent(event, api) {
    try {
      if (event.event === "command" && event.command === "toggle-git") {
        if (shown) await close(api);
        else await open(api);
        return;
      }
      if (event.event === "state") {
        const next = follow(event.state);
        if (next === cwd) return;
        cwd = next;
        if (!shown) return;
        status = { text: "" };
        await refresh();
      } else if (event.event === "ui" && event.data.panel === "git") {
        const data = event.data;
        if (data.kind === "select") { selected = data.item; return; }
        if (data.kind === "change") { message = data.value; return; }
        if (data.kind === "activate" && root) {
          await api.openTerminal({ program: GIT, args: ["diff", "HEAD", "--", data.item], cwd: root });
          return;
        }
        let control = data.kind === "click" ? data.control : undefined;
        if (data.kind === "key") {
          const move = MOVES[data.key];
          if (move) {
            if (changes.length) await api.navigate({ panel: "git", control: "changes", to: move });
            return;
          }
          if (data.key === "C") return await api.focusPanel({ panel: "git", control: "message" });
          control = ACTIONS[data.key];
        }
        if (!control) return;
        status = { text: "" };
        switch (control) {
          case "close":
            await close(api);
            return;
          case "stage":
            if (selected) await git(["add", "--", selected]);
            break;
          case "unstage":
            if (selected) await git(["restore", "--staged", "--", selected]);
            break;
          case "stage-all":
            await git(["add", "--all"]);
            break;
          case "refresh":
            break;
          case "commit": {
            if (!message.trim()) { status = { text: "Write a commit message first.", color: "warning" }; break; }
            busy = true;
            await show(api);
            try {
              const out = await git(["commit", "-m", message]);
              message = "";
              status = { text: out.split("\n")[0], color: "accent" };
            } finally {
              busy = false;
            }
            break;
          }
        }
        await refresh();
      } else {
        return;
      }
      await show(api);
    } catch (e) {
      status = { text: String(e instanceof Error ? e.message : e), color: "error" };
      await refresh();
      await show(api);
    }
  },
});
