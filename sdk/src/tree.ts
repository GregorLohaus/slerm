import type { Element, TreeRow, UIEvent } from "./generated.js";

/** Where a `Tree` gets its items. Children load lazily, when a row is expanded. */
export interface TreeSource<T> {
  roots(): T[] | Promise<T[]>;
  children(item: T): T[] | Promise<T[]>;
  /** A stable identity, such as a path. Expansion and selection follow keys across reloads. */
  key(item: T): string;
  label(item: T): string;
  expandable(item: T): boolean;
}

export type TreeEvent<T> =
  | { kind: "select"; item: T }
  | { kind: "toggle"; item: T; expanded: boolean }
  | { kind: "activate"; item: T };

export type TreeElement = Extract<Element, { type: "tree" }>;

// Host limits for a tree control.
const MAX_ROWS = 8192;
const MAX_DEPTH = 32;

const printable = (text: string) =>
  text.replace(/[\u0000-\u001f\u007f]/g, c => `\\u${c.charCodeAt(0).toString(16).padStart(4, "0")}`);

/**
 * State for a native tree control: loaded children, expansion, and selection.
 * Render it with `element()` inside `setPanel`, and pass UI events to `handle()`.
 */
export class Tree<T> {
  /** Whether the last `element()` call dropped rows beyond the host limit. */
  truncated = false;
  #roots: T[] = [];
  #children = new Map<string, T[]>();
  #expanded = new Set<string>();
  #selected: string | undefined;
  // Rows carry short IDs so long keys such as paths do not bloat each panel update.
  #rowIds = new Map<string, string>();
  #visible = new Map<string, { item: T; depth: number }>();
  #nextId = 0;

  constructor(readonly id: string, readonly source: TreeSource<T>) {}

  get selected(): T | undefined {
    for (const { item } of this.#visible.values()) {
      if (this.source.key(item) === this.#selected) return item;
    }
    return undefined;
  }

  /** Reload roots and expanded rows, keeping expansion and selection for keys that remain. */
  async load(): Promise<void> {
    const roots = await this.source.roots();
    const children = new Map<string, T[]>();
    await this.#fill(roots, 0, children);
    this.#roots = roots;
    this.#children = children;
  }

  /** Forget expansion and selection, then load. Use this when the tree shows new roots. */
  async reset(): Promise<void> {
    this.#expanded.clear();
    this.#selected = undefined;
    await this.load();
  }

  element(): TreeElement {
    const rows: TreeRow[] = [];
    const ids = new Map<string, string>();
    this.#visible = new Map();
    this.truncated = false;
    const visit = (items: T[], depth: number) => {
      for (const item of items) {
        if (rows.length >= MAX_ROWS) { this.truncated = true; return; }
        const key = this.source.key(item);
        const id = this.#rowIds.get(key) ?? String(this.#nextId++);
        ids.set(key, id);
        this.#visible.set(id, { item, depth });
        const children = this.#children.get(key);
        const expandable = depth < MAX_DEPTH && this.source.expandable(item);
        const expanded = expandable && children !== undefined && this.#expanded.has(key);
        rows.push({ id, label: printable(this.source.label(item)), depth, expandable, expanded });
        if (expanded) visit(children, depth + 1);
      }
    };
    visit(this.#roots, 0);
    this.#rowIds = ids;
    const selected = this.#selected === undefined ? undefined : ids.get(this.#selected);
    return { type: "tree", id: this.id, rows, selected: selected ?? null };
  }

  /**
   * Apply a UI event for this control. Returns what happened, or undefined if the
   * event is for another control. Re-render after toggles; the host already shows
   * selection changes. Check `event.panel` first when several panels use one ID.
   */
  async handle(event: UIEvent): Promise<TreeEvent<T> | undefined> {
    if (event.control !== this.id || !("item" in event)) return undefined;
    const row = this.#visible.get(event.item);
    if (!row) return undefined;
    const key = this.source.key(row.item);
    this.#selected = key;
    if (event.kind !== "toggle") return { kind: event.kind, item: row.item };
    if (event.expanded) {
      // Load errors reach the caller and leave the row collapsed.
      const children = await this.source.children(row.item);
      const loaded = new Map(this.#children).set(key, children);
      this.#expanded.add(key);
      // Reopen descendants that were expanded before this row collapsed.
      await this.#fill(children, row.depth + 1, loaded);
      this.#children = loaded;
    } else {
      this.#expanded.delete(key);
    }
    return { kind: "toggle", item: row.item, expanded: event.expanded };
  }

  async #fill(items: T[], depth: number, into: Map<string, T[]>): Promise<void> {
    for (const item of items) {
      const key = this.source.key(item);
      if (!this.#expanded.has(key) || depth >= MAX_DEPTH || !this.source.expandable(item)) continue;
      let children: T[];
      try {
        children = await this.source.children(item);
      } catch {
        // A folder that vanished or became unreadable collapses on reload.
        this.#expanded.delete(key);
        continue;
      }
      into.set(key, children);
      await this.#fill(children, depth + 1, into);
    }
  }
}
