# Slerm TypeScript plugin SDK

Slerm runs JavaScript bundles in **embedded Boa**, with a separate worker thread
and context for each plugin. End users do not need Node.js or Bun. Plugin authors
use TypeScript and a bundler during development.

## Try the example

From the Slerm repository root:

```sh
npm ci --prefix sdk
npm run example --prefix sdk
```

Add this to your Slerm configuration, replacing the absolute path:

```toml
[[plugins]]
id = "panes"
path = "/absolute/path/to/slerm/sdk/dist/panes.js"
```

Start Slerm and choose **Plugins → panes → Toggle pane dashboard** (or bind a key
to it, below). The dashboard is a sidebar with a live table of panes (click to
focus, double-click to zoom), a preview of the active pane's terminal text, a box
for sending commands to one or every pane, and a menu of built-in actions.
`dist/filetree.js` (a file browser) and `dist/git.js` (a git panel) are configured
the same way.

Bundle paths can also be relative to the configuration file's directory. Plugins
load at startup; restart Slerm after changing a bundle or the plugin configuration.

## Write a plugin

Build the SDK with `npm run build --prefix sdk`. In a separate plugin project,
install the local package with `npm install /absolute/path/to/slerm/sdk` and add
TypeScript and esbuild as development dependencies. The package has not been
published to a registry.

```ts
import { definePlugin } from "@slerm/plugin-sdk";

definePlugin({
  async activate(slerm) {
    await slerm.registerCommand({
      command: { id: "scratch", title: "New scratch pane" },
    });
  },
  async onEvent(event, slerm) {
    if (event.event === "command" && event.command === "scratch") {
      await slerm.execute({ action: "new_floating" });
    }
  },
});
```

Type-check your entry point, then bundle it:

```sh
npx tsc --noEmit --strict --target es2022 --module nodenext plugin.ts
npx esbuild plugin.ts --bundle --format=iife --platform=neutral --target=es2022 --outfile=plugin.js
```

Point a `[[plugins]]` entry at `plugin.js`. Each bundle calls `definePlugin` once.
All API calls return promises that resolve after Slerm applies the operation.
Events invoke `onEvent(event, slerm)`; callbacks can be async.

## Generated API

The Rust definitions in [`src/plugin_api.rs`](../src/plugin_api.rs) are the source
of truth. A Rust method list declares parameters and results; `ts-rs` generates
`src/generated.ts`, and TypeScript derives the client method signatures from those
types. Actions come from Slerm's existing Rust action enum. State, UI elements,
and events also use the same Rust types for serialization and type generation.
The generic runtime bridge does not duplicate a method list or handwritten types.

Regenerate after changing the Rust API:

```sh
nix develop -c cargo run -- --generate-plugin-sdk
npm test --prefix sdk
nix develop -c cargo test
nix develop -c cargo test bundled_typescript_example_runs_in_boa -- --ignored
```

Without Nix, run Cargo in an environment containing the project's Rust and Zig
build dependencies. Commit `sdk/src/generated.ts` with API changes. A Rust test
fails if the checked-in bindings differ from the schema. Bump `API_VERSION` for
incompatible changes; Slerm rejects bundles built for another API version.

| Method | Purpose |
| --- | --- |
| `getState()` | Read tabs, panes, titles, focus, floating state, and zoom |
| `execute({ action })` | Execute a built-in pane, tab, clipboard, or settings action |
| `focusPane({ pane })`, `focusTab({ tab })` | Focus by ID from `getState()` |
| `readTerminal({ pane })` | Read a plain-text screen and scrollback snapshot, up to 128 KiB |
| `writeTerminal({ pane, text })` | Send raw PTY input; include `\r` to submit a shell command |
| `openTerminal({ program, args, cwd })` | Split the active pane and run a program in a new pane |
| `registerCommand({ command })`, `unregisterCommand({ command })` | Manage entries in the Plugins menu |
| `setPanel({ panel })`, `removePanel({ panel })` | Create, replace, or remove a native panel |
| `focusPanel({ panel, control? })` | Give keyboard focus to a control, or the panel's first list or text field |
| `navigate({ panel, control, to })` | Move a tree or table as its keys would: `next`, `previous`, `first`, `last`, `pageDown`, `pageUp`, `left`, `right`, `activate` |
| `reportError({ message })` | Display an error in Slerm |

## Filesystem

`import { fs } from "@slerm/plugin-sdk"` gives filesystem access. Like the API
above, its methods are generated from Rust (`FsOperation` in `plugin_api.rs`),
but they run on the plugin's own worker thread instead of the UI thread. Paths
must be absolute; `PaneState.cwd` gives each pane's working directory.

| Method | Purpose |
| --- | --- |
| `readDir({ path })` | List entries (`name`, `path`, `kind`, `symlink`), sorted by name |
| `stat({ path })` | `kind`, `symlink`, `size`, and `modified` in epoch milliseconds |
| `readFile({ path })`, `writeFile({ path, text })` | Read or write UTF-8 text, up to 16 MiB |
| `createDir({ path })`, `remove({ path })`, `rename({ from, to })` | Create directories recursively; remove files or whole directories |

## System, HTTP, and timers

`import { system } from "@slerm/plugin-sdk"` reaches outside Slerm. `run` and
`fetch` execute on their own threads: the promise resolves when they finish, and
the plugin keeps handling events meanwhile. At most 16 run at once per plugin.

| Method | Purpose |
| --- | --- |
| `run({ program, args?, cwd, stdin?, timeout? })` | Run a process to completion: `{ status, stdout, stderr, timedOut }`. `timeout` is in milliseconds (default 30 s, at most 10 min); output is capped at 8 MiB per stream |
| `fetch({ url, method?, headers?, body?, timeout? })` | An HTTP(S) request: `{ status, headers, body }`. Every HTTP status resolves; network errors reject |
| `env({ name })` | An environment variable of the Slerm process, or `null` |

Processes see the configuration's `[env]` table on top of Slerm's environment,
and `env` reads it first. Apps started from the Dock or Finder get a minimal
`PATH`, so set one there:

```toml
[env]
PATH = "/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"
```

`setTimeout`, `setInterval`, `clearTimeout`, and `clearInterval` work as in
browsers. Intervals repeat no faster than every 10 ms.

## Key bindings

Bind a key to any registered plugin command in Slerm's configuration:

```toml
[keys]
"Super+G" = "plugin:git/toggle-git"
```

The Plugins menu shows the binding next to the command.

Command and panel IDs are scoped to the plugin. Control IDs must be unique within
a panel. Pane and tab IDs come from Slerm and should be treated as opaque numbers.

Events are discriminated by `event.event`:

- `command`: `event.command` is the selected command ID.
- `state`: `event.state` is a snapshot when workspace metadata changes. Terminal
  output changes alone do not produce this event; use `readTerminal` for snapshots.
- `ui`: `event.data` contains the panel and control IDs and a `kind`: `click`,
  `change` (with the text or option `value`), `check` (with `checked`), `number`
  (with `value`), `key` (with the declared `key` and the focused list as
  `control`), or `select`, `toggle` (with `expanded`), and `activate` for
  table rows and tree nodes (with the row or node ID as `item`).

## Native UI

`setPanel` renders a floating window, or a resizable sidebar with
`placement: "left"` or `"right"`. Its `elements` are generated from the Rust
`Element` enum:

| Element | Purpose | Events |
| --- | --- | --- |
| `text` | A label, with optional `style` (`heading`, `strong`, `weak`, `small`, `monospace`) and theme `color` (`accent`, `warning`, `error`) | |
| `button` | A push button | `click` |
| `input` | A single-line text field | `change` |
| `textArea` | Multi-line text; `code` for monospace, `readonly`, and `follow` to stay scrolled to the end like a log | `change` |
| `checkbox` | An on/off toggle | `check` |
| `slider` | A number between `min` and `max`, with an optional `step` | `number` on release |
| `select` | A dropdown of `options` | `change` |
| `progress` | A bar from 0 to 1, labeled with `text` or a percentage | |
| `spinner`, `separator` | Busy indicator, divider | |
| `row` | Lays out nested elements horizontally | |
| `section` | A collapsible group of nested elements; `open` sets its initial state | |
| `table` | Selectable rows of text cells under column headers | `select`, `activate` |
| `tree` | Nested, lazily loaded nodes with icons | `select`, `toggle`, `activate` |

### Keyboard

Focused tables and trees handle arrow keys, Home, End, Page Up and Down, and
Enter; Escape returns focus to the terminal. A panel's `keys` lists further
shortcuts, in the configuration's syntax (`"J"`, `"Shift+G"`, `"Ctrl+D"`), which
arrive as `key` events while one of its lists has focus. Text fields keep their
typing. Combine them with `navigate` for any keymap, such as Vim's:

```ts
const moves: Record<string, Navigation> = { J: "next", K: "previous", H: "left", L: "right" };
await slerm.setPanel({ panel: { id: "files", title: "Files", keys: Object.keys(moves), elements } });
await slerm.focusPanel({ panel: "files", control: "tree" });
// In onEvent:
if (event.event === "ui" && event.data.kind === "key") {
  await slerm.navigate({ panel: "files", control: "tree", to: moves[event.data.key] });
}
```

Slerm's own key bindings keep working while a plugin list has focus.

Values a user edits (`value`, `checked`, a table's `selected`) change locally,
so a plugin only needs to send the panel again when its own state changes. A
text field being typed in keeps its local value when the panel is replaced.
Double-clicking a table or tree row activates it.

A `tree` takes nested `nodes`, each with an `id`, a `label`, an optional `icon`
glyph (Nerd Font icons work when the terminal font has them), and `children`.
Slerm owns expansion and selection, keyed by node ID, so they survive updates and
need no round trip. A node with `branch: true` and no `children` is unloaded:
expanding it sends a `toggle` event, and the plugin sends the panel again with
its children. Double-clicking or pressing Enter on a leaf sends `activate`.

See [`examples/panes.ts`](examples/panes.ts);
[`examples/filetree.ts`](examples/filetree.ts), a file browser sidebar that
follows the active pane's directory and opens files in `hx`; and
[`examples/git.ts`](examples/git.ts), a git sidebar that refreshes on a timer,
stages files, commits, and opens diffs; and
[`examples/hackernews.ts`](examples/hackernews.ts), a Hacker News reader that
fetches feeds and shows comment threads as a tree.

## Runtime boundaries

The runtime supports ECMAScript, promises, and timers. Besides `fs` and
`system`, the host does not expose Node.js APIs or a DOM. Bundle imports ahead of
time and use libraries compatible with this environment. Report diagnostics with
`reportError`; `console` is not installed.

Plugins are trusted local extensions with the user's privileges: they can run
programs, make network requests, and read and write any file the user can.
Boa contexts run in the same OS process as Slerm; worker threads and execution
limits are not an OS security sandbox. Slerm bounds loops, recursion, promise jobs,
request waits, bundle/message sizes, and UI counts. An uncaught callback error
stops that plugin and removes its commands and panels. Host-operation errors
reject the API promise and can be handled by the plugin.

Implementation references: [Boa](https://boajs.dev/docs/intro) and
[ts-rs](https://docs.rs/ts-rs/latest/ts_rs/).
