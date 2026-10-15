import { definePlugin, system, type Element, type Navigation, type Slerm, type TextColor, type TreeNode } from "../src/index.js";

// Algolia's Hacker News API needs no key, returns a feed in one request, and a
// story's whole comment thread, already nested, in another.
const API = "https://hn.algolia.com/api/v1";
const FEEDS: Record<string, string> = {
  "Front page": "search?tags=front_page&hitsPerPage=50",
  "Newest": "search_by_date?tags=story&hitsPerPage=50",
  "Ask HN": "search_by_date?tags=ask_hn&hitsPerPage=50",
  "Show HN": "search_by_date?tags=show_hn&hitsPerPage=50",
};
const REFRESH_MS = 5 * 60 * 1000;
// Keys sent while the story table or comment tree has focus.
const MOVES: Record<string, Navigation> = {
  J: "next", K: "previous", H: "left", L: "right", O: "activate",
  "Shift+G": "last", "Ctrl+D": "pageDown", "Ctrl+U": "pageUp",
};
const KEYS = [...Object.keys(MOVES), "G", "S", "C", "B", "F", "R", "Q"];

type Story = {
  objectID: string; title: string; url: string | null; author: string;
  points: number | null; num_comments: number | null; created_at_i: number; story_text: string | null;
};
type Comment = { id: number; author: string | null; text: string | null; created_at_i: number; children: Comment[] };

let shown = false;
let feed = "Front page";
let stories: Story[] = [];
let selected: string | undefined;
let thread: { story: Story; comments: Comment[] } | undefined;
const comments = new Map<string, Comment>();
let reading: Comment | undefined;
let loading = "";
let status: { text: string; color?: TextColor } = { text: "" };
let timer: number | undefined;
let lastG = 0;

async function get<T>(path: string): Promise<T> {
  const response = await system.fetch({ url: `${API}/${path}`, headers: { accept: "application/json" }, timeout: 15000 });
  if (response.status !== 200) throw new Error(`Hacker News returned HTTP ${response.status}`);
  return JSON.parse(response.body) as T;
}

// Comments arrive as HTML: paragraphs, links, italics, and entities.
function text(html: string | null): string {
  return (html ?? "")
    .replace(/<p>/g, "\n\n")
    .replace(/<br\s*\/?>/g, "\n")
    .replace(/<[^>]+>/g, "")
    .replace(/&#x([0-9a-f]+);/gi, (_, hex) => String.fromCodePoint(parseInt(hex, 16)))
    .replace(/&#(\d+);/g, (_, dec) => String.fromCodePoint(Number(dec)))
    .replace(/&quot;/g, "\"").replace(/&gt;/g, ">").replace(/&lt;/g, "<").replace(/&amp;/g, "&")
    .trim();
}

function ago(seconds: number): string {
  const elapsed = Math.max(0, Date.now() / 1000 - seconds);
  for (const [unit, size] of [["d", 86400], ["h", 3600], ["m", 60]] as const) {
    if (elapsed >= size) return `${Math.floor(elapsed / size)}${unit}`;
  }
  return "now";
}

const domain = (url: string | null) => url?.match(/^https?:\/\/(?:www\.)?([^/]+)/)?.[1] ?? "news.ycombinator.com";
const link = (story: Story) => story.url ?? `https://news.ycombinator.com/item?id=${story.objectID}`;
const current = () => stories.find(story => story.objectID === selected);

function nodes(list: Comment[]): TreeNode[] {
  return list
    .filter(comment => comment.text || comment.children.length)
    .map(comment => {
      comments.set(String(comment.id), comment);
      const first = text(comment.text).split("\n")[0] || "[deleted]";
      const replies = comment.children.length ? ` (${comment.children.length})` : "";
      return {
        id: String(comment.id),
        label: `${comment.author ?? "[deleted]"}${replies} · ${first}`,
        children: comment.children.length ? nodes(comment.children) : undefined,
      };
    });
}

async function show(api: Slerm) {
  if (!shown) return;
  const story = current();
  comments.clear();
  const elements: Element[] = [
    { type: "row", elements: [
      { type: "select", id: "feed", value: feed, options: Object.keys(FEEDS) },
      { type: "button", id: "refresh", label: "Refresh" },
      ...(loading ? [{ type: "spinner" } as const, { type: "text", text: loading, style: "small" } as const] : []),
    ] },
    { type: "text", text: "j/k move · o open · h/l fold · s stories · c comments · b browser · f feed · q close", style: "small" },
    ...(status.text ? [{ type: "text", text: status.text, color: status.color } as const] : []),
    { type: "section", id: "stories-section", title: feed, elements: [
      { type: "table", id: "stories", columns: ["", "", "Age", "Title"], selected,
        rows: stories.map(story => ({ id: story.objectID, cells: [
          String(story.points ?? 0), String(story.num_comments ?? 0), ago(story.created_at_i), story.title,
        ] })) },
    ] },
  ];
  if (story) {
    elements.push({ type: "section", id: "story-section", title: "Story", elements: [
      { type: "text", text: story.title, style: "strong" },
      { type: "text", text: `${story.points ?? 0} points by ${story.author} · ${ago(story.created_at_i)} ago · ${domain(story.url)}`, style: "small" },
      ...(story.story_text ? [{ type: "text", text: text(story.story_text) } as const] : []),
      { type: "row", elements: [
        { type: "button", id: "open", label: "Open link" },
        { type: "button", id: "discussion", label: "Open on HN" },
        { type: "button", id: "comments", label: thread?.story.objectID === story.objectID ? "Reload comments" : "Load comments" },
      ] },
    ] });
  }
  if (thread) {
    // The tree fills the remaining height, so the reader goes above it.
    elements.push({ type: "section", id: "comments-section", title: `Comments · ${thread.story.title}`, elements: [
      ...(reading ? [{ type: "textArea", id: "reader", rows: 8, readonly: true,
        value: `${reading.author ?? "[deleted]"} · ${ago(reading.created_at_i)} ago\n\n${text(reading.text)}` } as const] : []),
      { type: "tree", id: "thread", nodes: nodes(thread.comments) },
    ] });
  }
  await api.setPanel({ panel: { id: "hn", title: "Hacker News", placement: "right", keys: KEYS, elements } });
}

async function load(api: Slerm) {
  loading = `Loading ${feed}…`;
  await show(api);
  try {
    stories = (await get<{ hits: Story[] }>(FEEDS[feed])).hits.filter(story => story.title);
    if (!current()) selected = stories[0]?.objectID;
    status = { text: "" };
  } finally {
    loading = "";
  }
}

async function loadComments(api: Slerm, story: Story) {
  loading = "Loading comments…";
  await show(api);
  try {
    const item = await get<{ children: Comment[] }>(`items/${story.objectID}`);
    thread = { story, comments: item.children };
    reading = undefined;
  } finally {
    loading = "";
  }
  await show(api);
  await api.focusPanel({ panel: "hn", control: "thread" });
}

// macOS opens URLs with `open`, Linux desktops with `xdg-open`.
async function browse(url: string) {
  for (const program of ["open", "xdg-open"]) {
    try {
      const out = await system.run({ program, args: [url], cwd: "/" });
      if (out.status === 0) return;
    } catch {}
  }
  throw new Error("Cannot open a browser");
}

async function hide(api: Slerm) {
  shown = false;
  clearInterval(timer);
  await api.removePanel({ panel: "hn" });
}

definePlugin({
  async activate(api) {
    await api.registerCommand({ command: { id: "toggle-hn", title: "Toggle Hacker News" } });
  },
  async onEvent(event, api) {
    if (event.event === "command" && event.command === "toggle-hn") {
      if (shown) return await hide(api);
      shown = true;
      timer = setInterval(async () => { await load(api).catch(() => {}); await show(api); }, REFRESH_MS);
      try { await load(api); } catch (e) { status = { text: String(e), color: "error" }; }
      await show(api);
      await api.focusPanel({ panel: "hn", control: "stories" });
      return;
    }
    if (event.event !== "ui" || event.data.panel !== "hn") return;
    const data = event.data;
    try {
      switch (data.kind) {
        case "select":
          if (data.control === "stories") selected = data.item;
          else if (data.control === "thread") reading = comments.get(data.item);
          break;
        case "activate":
          if (data.control === "stories") {
            selected = data.item;
            const story = current();
            if (story) await loadComments(api, story);
            return;
          }
          return;
        case "change":
          if (data.control !== "feed") return;
          feed = data.value;
          selected = undefined;
          await load(api);
          await api.focusPanel({ panel: "hn", control: "stories" });
          break;
        case "click": {
          const story = current();
          if (data.control === "refresh") await load(api);
          if (data.control === "open" && story) await browse(link(story));
          if (data.control === "discussion" && story) await browse(`https://news.ycombinator.com/item?id=${story.objectID}`);
          if (data.control === "comments" && story) return await loadComments(api, story);
          break;
        }
        case "key": {
          const move = MOVES[data.key];
          if (move) return await api.navigate({ panel: "hn", control: data.control, to: move });
          if (data.key === "G") {
            // gg jumps to the top: two presses within half a second.
            const now = Date.now();
            if (now - lastG > 500) { lastG = now; return; }
            lastG = 0;
            return await api.navigate({ panel: "hn", control: data.control, to: "first" });
          }
          if (data.key === "S") return await api.focusPanel({ panel: "hn", control: "stories" });
          if (data.key === "C" && thread) return await api.focusPanel({ panel: "hn", control: "thread" });
          if (data.key === "Q") return await hide(api);
          const story = current();
          if (data.key === "B" && story) await browse(link(story));
          if (data.key === "R") await load(api);
          if (data.key === "F") {
            const names = Object.keys(FEEDS);
            feed = names[(names.indexOf(feed) + 1) % names.length];
            selected = undefined;
            await load(api);
          }
          break;
        }
        default:
          return;
      }
    } catch (e) {
      status = { text: String(e instanceof Error ? e.message : e), color: "error" };
    }
    await show(api);
  },
});
