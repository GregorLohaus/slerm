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
id = "workspace"
path = "/absolute/path/to/slerm/sdk/dist/workspace.js"
```

Start Slerm. The example opens **Workspace tools**, with controls to create a
floating pane, run a shell command, and inspect terminal text. Reopen the panel
from **Plugins → workspace → Show workspace tools**.

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

Command and panel IDs are scoped to the plugin. Control IDs must be unique within
a panel. Pane and tab IDs come from Slerm and should be treated as opaque numbers.

Events are discriminated by `event.event`:

- `command`: `event.command` is the selected command ID.
- `state`: `event.state` is a snapshot when workspace metadata changes. Terminal
  output changes alone do not produce this event; use `readTerminal` for snapshots.
- `ui`: `event.data` contains the panel/control IDs and a `kind`: `click`
  (button), `change` (input, with `value`), or `select`, `toggle` (with
  `expanded`), and `activate` for tree nodes (with the node ID as `item`).

## Native UI

`setPanel` renders a floating window, or a resizable sidebar with
`placement: "left"` or `"right"`. Its `elements` accept `text`, `button`,
`input`, `separator`, `row` (lays out nested elements horizontally), and `tree`.
Inputs retain edits until replaced by another `setPanel` call. UI focus receives
typing without forwarding it to the terminal.

A `tree` takes nested `nodes`, each with an `id`, a `label`, an optional `icon`
glyph (Nerd Font icons work when the terminal font has them), and `children`.
Slerm owns expansion and selection, keyed by node ID, so they survive updates and
need no round trip. A node with `branch: true` and no `children` is unloaded:
expanding it sends a `toggle` event, and the plugin sends the panel again with
its children. Double-clicking or pressing Enter on a leaf sends `activate`.

See [`examples/workspace.ts`](examples/workspace.ts) and
[`examples/filetree.ts`](examples/filetree.ts), a file browser sidebar that
follows the active pane's directory and opens files in `hx`.

## Runtime boundaries

The runtime supports ECMAScript and promises. Besides `fs`, the host does not
expose Node.js APIs, a DOM, network APIs, or timers. Bundle imports ahead of
time and use libraries compatible with this environment. Report diagnostics with
`reportError`; `console` is not installed.

Plugins are trusted local extensions: terminal writes can run shell commands, and
`fs` can read and write any file the user can.
Boa contexts run in the same OS process as Slerm; worker threads and execution
limits are not an OS security sandbox. Slerm bounds loops, recursion, promise jobs,
request waits, bundle/message sizes, and UI counts. An uncaught callback error
stops that plugin and removes its commands and panels. Host-operation errors
reject the API promise and can be handled by the plugin.

Implementation references: [Boa](https://boajs.dev/docs/intro) and
[ts-rs](https://docs.rs/ts-rs/latest/ts_rs/).
