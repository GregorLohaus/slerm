//! The source of truth for the plugin API and its generated TypeScript bindings.
use crate::config::Action;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

pub const API_VERSION: u32 = 1;

// Declare each method, its parameters and its result once. The enum drives Rust
// dispatch; the generated result map drives the TypeScript client's signatures.
macro_rules! plugin_api {
    ($operation:ident, $results:ident; $($(#[$doc:meta])* $variant:ident => $method:ident { $($(#[$attr:meta])* $arg:ident: $ty:ty),* } -> $result:ty;)+) => {
        #[derive(Debug, Deserialize, TS)]
        // Parameters sit under `params`, so a parameter may itself be named `method`.
        #[serde(tag = "method", content = "params", rename_all = "camelCase", deny_unknown_fields)]
        pub enum $operation {
            $($(#[$doc])* $variant { $($(#[$attr])* $arg: $ty),* },)+
        }
        #[derive(TS)]
        #[allow(dead_code, non_snake_case)]
        pub struct $results { $(pub $method: $result,)+ }
    };
}
// Host operations run on the UI thread and can touch panes, tabs, and panels.
plugin_api! { Operation, Results;
    GetState => getState {} -> State;
    Execute => execute { action: Action } -> ();
    FocusPane => focusPane { #[ts(type = "number")] pane: u64 } -> ();
    FocusTab => focusTab { #[ts(type = "number")] tab: u64 } -> ();
    ReadTerminal => readTerminal { #[ts(type = "number")] pane: u64 } -> String;
    WriteTerminal => writeTerminal { #[ts(type = "number")] pane: u64, text: String } -> ();
    OpenTerminal => openTerminal { program: String, args: Vec<String>, cwd: String } -> PaneState;
    RegisterCommand => registerCommand { command: PluginCommand } -> ();
    UnregisterCommand => unregisterCommand { command: String } -> ();
    SetPanel => setPanel { panel: Panel } -> ();
    RemovePanel => removePanel { panel: String } -> ();
    /// Give keyboard focus to a control, or to the panel's first list or text field.
    FocusPanel => focusPanel { panel: String, #[serde(default)] #[ts(optional)] control: Option<String> } -> ();
    /// Move a tree or table as its arrow keys would, emitting the same UI events.
    Navigate => navigate { panel: String, control: String, to: Navigation } -> ();
    ReportError => reportError { message: String } -> ();
}

// Filesystem operations run synchronously on the plugin's own worker thread, so
// slow disks never stall rendering. Paths must be absolute.
plugin_api! { FsOperation, FsResults;
    ReadDir => readDir { path: String } -> Vec<DirEntry>;
    Stat => stat { path: String } -> FileStat;
    ReadFile => readFile { path: String } -> String;
    WriteFile => writeFile { path: String, text: String } -> ();
    CreateDir => createDir { path: String } -> ();
    Remove => remove { path: String } -> ();
    Rename => rename { from: String, to: String } -> ();
}

// System operations reach outside Slerm. `run` and `fetch` resolve when they
// finish, on their own threads, so the plugin keeps handling events meanwhile.
plugin_api! { SystemOperation, SystemResults;
    Run => run {
        program: String,
        #[serde(default)] #[ts(optional)] args: Option<Vec<String>>,
        cwd: String,
        #[serde(default)] #[ts(optional)] stdin: Option<String>,
        /// Milliseconds before the process is killed; defaults to 30 seconds.
        #[serde(default)] #[ts(optional)] timeout: Option<f64>
    } -> RunOutput;
    Fetch => fetch {
        url: String,
        #[serde(default)] #[ts(optional)] method: Option<String>,
        #[serde(default)] #[ts(optional)] headers: Option<BTreeMap<String, String>>,
        #[serde(default)] #[ts(optional)] body: Option<String>,
        /// Milliseconds for the whole request; defaults to 30 seconds.
        #[serde(default)] #[ts(optional)] timeout: Option<f64>
    } -> FetchResponse;
    Env => env { name: String } -> Option<String>;
}

/// `status` is null when a signal ended the process, including a timeout.
#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Any HTTP status resolves; only network and protocol failures reject.
#[derive(Debug, Serialize, TS)]
pub struct FetchResponse {
    pub status: u16,
    /// Lowercase names; repeated headers are joined with ", ".
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

#[derive(Clone, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct State {
    #[ts(type = "number | null")]
    pub active_tab: Option<u64>,
    pub tabs: Vec<TabState>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TabState {
    #[ts(type = "number")]
    pub id: u64,
    #[ts(type = "number")]
    pub active_pane: u64,
    pub floats_visible: bool,
    pub zoomed: bool,
    pub panes: Vec<PaneState>,
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct PaneState {
    #[ts(type = "number")]
    pub id: u64,
    pub title: String,
    pub floating: bool,
    pub exited: bool,
    pub cwd: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    File,
    Directory,
    Other,
}

/// `kind` follows symlinks; `symlink` reports whether the entry itself is one.
#[derive(Debug, Serialize, TS)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub kind: FileKind,
    pub symlink: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct FileStat {
    pub kind: FileKind,
    pub symlink: bool,
    #[ts(type = "number")]
    pub size: u64,
    /// Milliseconds since the Unix epoch, when the platform reports it.
    #[ts(type = "number | null")]
    pub modified: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PluginCommand {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    pub id: String,
    pub title: String,
    pub elements: Vec<Element>,
    #[serde(default)]
    #[ts(optional)]
    pub placement: Option<PanelPlacement>,
    /// Shortcuts sent as `key` events while a list in this panel has focus,
    /// such as `"J"`, `"Shift+G"`, or `"Ctrl+D"`. Text fields keep their typing.
    #[serde(default)]
    #[ts(optional)]
    pub keys: Option<Vec<String>>,
    /// Expansion and selection belong to the host and survive panel updates.
    #[serde(skip)]
    #[ts(skip)]
    pub lists: std::collections::HashMap<String, crate::plugin_ui::ListState>,
    /// The list control that had keyboard focus in the last frame.
    #[serde(skip)]
    #[ts(skip)]
    pub focused: Option<String>,
    /// A pending `focusPanel`: a control ID, or None for the first focusable control.
    #[serde(skip)]
    #[ts(skip)]
    pub focus_request: Option<Option<String>>,
    /// The text field the user is typing in, whose local value outranks updates.
    #[serde(skip)]
    #[ts(skip)]
    pub editing: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum Navigation {
    Next,
    Previous,
    First,
    Last,
    PageDown,
    PageUp,
    /// Collapse a tree row, or move to its parent.
    Left,
    /// Expand a tree row, or move to its first child.
    Right,
    Activate,
}

#[derive(Clone, Copy, Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum PanelPlacement {
    Floating,
    Left,
    Right,
}

/// A node in a `tree` element. A node with `children` or `branch: true` can
/// expand. Expanding emits a `toggle` event, so a plugin can send children
/// that load on demand. Node IDs must be unique within the tree and stable
/// across updates: Slerm keeps expansion and selection by ID.
#[derive(Clone, Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TreeNode {
    pub id: String,
    pub label: String,
    /// A short glyph shown before the label, such as a Nerd Font icon.
    #[serde(default)]
    #[ts(optional)]
    pub icon: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub branch: Option<bool>,
    #[serde(default)]
    #[ts(optional)]
    pub children: Option<Vec<TreeNode>>,
}

impl TreeNode {
    pub fn is_branch(&self) -> bool {
        self.branch.unwrap_or(false) || self.children.is_some()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum TextStyle {
    Heading,
    Strong,
    Weak,
    Small,
    Monospace,
}

/// Theme colors, so plugin text follows the active theme.
#[derive(Clone, Copy, Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum TextColor {
    Accent,
    Warning,
    Error,
}

#[derive(Clone, Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TableRow {
    pub id: String,
    pub cells: Vec<String>,
}

/// Controls with an `id` emit UI events. Values a user edits (`value`,
/// `checked`, `selected`) change locally until the plugin sends the panel again.
#[derive(Clone, Debug, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Element {
    Text {
        text: String,
        #[serde(default)]
        #[ts(optional)]
        style: Option<TextStyle>,
        #[serde(default)]
        #[ts(optional)]
        color: Option<TextColor>,
    },
    /// Emits `click`.
    Button {
        id: String,
        label: String,
    },
    /// A single-line text field. Emits `change`.
    Input {
        id: String,
        label: String,
        value: String,
    },
    /// A multi-line text field. Emits `change` unless read-only.
    TextArea {
        id: String,
        value: String,
        /// Visible lines; defaults to 6.
        #[serde(default)]
        #[ts(optional)]
        rows: Option<u32>,
        /// Monospace text, as in a code editor.
        #[serde(default)]
        #[ts(optional)]
        code: Option<bool>,
        #[serde(default)]
        #[ts(optional)]
        readonly: Option<bool>,
    },
    /// Emits `check`.
    Checkbox {
        id: String,
        label: String,
        checked: bool,
    },
    /// Emits `number` when the user releases the slider.
    Slider {
        id: String,
        #[serde(default)]
        #[ts(optional)]
        label: Option<String>,
        value: f64,
        min: f64,
        max: f64,
        #[serde(default)]
        #[ts(optional)]
        step: Option<f64>,
    },
    /// A dropdown. Emits `change` with the chosen option.
    Select {
        id: String,
        #[serde(default)]
        #[ts(optional)]
        label: Option<String>,
        value: String,
        options: Vec<String>,
    },
    /// A bar from 0 to 1, labeled with `text` or a percentage.
    Progress {
        value: f32,
        #[serde(default)]
        #[ts(optional)]
        text: Option<String>,
    },
    Spinner,
    Separator,
    /// Lays out its elements horizontally.
    Row {
        elements: Vec<Element>,
    },
    /// A collapsible group. Slerm remembers whether it is open.
    Section {
        id: String,
        title: String,
        /// Whether the section starts open; defaults to true.
        #[serde(default)]
        #[ts(optional)]
        open: Option<bool>,
        elements: Vec<Element>,
    },
    /// Rows of text cells. Emits `select` and `activate` with the row ID.
    Table {
        id: String,
        columns: Vec<String>,
        rows: Vec<TableRow>,
        #[serde(default)]
        #[ts(optional)]
        selected: Option<String>,
    },
    /// Emits `select`, `toggle`, and `activate` with the node ID.
    Tree {
        id: String,
        nodes: Vec<TreeNode>,
    },
}

#[derive(Clone, Debug, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UIEvent {
    Select {
        panel: String,
        control: String,
        item: String,
    },
    Activate {
        panel: String,
        control: String,
        item: String,
    },
    Toggle {
        panel: String,
        control: String,
        item: String,
        expanded: bool,
    },
    Click {
        panel: String,
        control: String,
    },
    Check {
        panel: String,
        control: String,
        checked: bool,
    },
    Number {
        panel: String,
        control: String,
        value: f64,
    },
    /// A declared shortcut; `control` is the focused list.
    Key {
        panel: String,
        control: String,
        key: String,
    },
    Change {
        panel: String,
        control: String,
        value: String,
    },
}

#[derive(Clone, Debug, Serialize, TS)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum Event {
    Command { command: String },
    State { state: State },
    Ui { data: UIEvent },
}

pub fn typescript() -> String {
    let mut output =
        String::from("// Generated by cargo run -- --generate-plugin-sdk. Do not edit.\n\n");
    output.push_str(&format!(
        "export const apiVersion = {API_VERSION} as const;\n\n"
    ));
    let config = ts_rs::Config::default();
    for declaration in [
        Action::decl(&config),
        Operation::decl(&config),
        Results::decl(&config),
        State::decl(&config),
        TabState::decl(&config),
        PaneState::decl(&config),
        FsOperation::decl(&config),
        FsResults::decl(&config),
        FileKind::decl(&config),
        DirEntry::decl(&config),
        FileStat::decl(&config),
        SystemOperation::decl(&config),
        SystemResults::decl(&config),
        RunOutput::decl(&config),
        FetchResponse::decl(&config),
        PluginCommand::decl(&config),
        Panel::decl(&config),
        PanelPlacement::decl(&config),
        Navigation::decl(&config),
        TreeNode::decl(&config),
        TextStyle::decl(&config),
        TextColor::decl(&config),
        TableRow::decl(&config),
        Element::decl(&config),
        UIEvent::decl(&config),
        Event::decl(&config),
    ] {
        output.push_str("export ");
        output.push_str(&declaration);
        output.push_str("\n\n");
    }
    output.push_str(r#"type Params<O, M> = Extract<O, { method: M }> extends { params: infer P } ? P : never;
type Client<O extends { method: string }, R extends Record<O["method"], unknown>> = {
  // ts-rs renders an empty parameter list as Record<symbol, never>.
  [M in O["method"]]: (...args: keyof Params<O, M> extends symbol ? [] : [params: Params<O, M>]) => Promise<R[M] extends null ? void : R[M]>;
};
export type Slerm = Client<Operation, Results>;
export type Fs = Client<FsOperation, FsResults>;
export type System = Client<SystemOperation, SystemResults>;
"#);
    output
}

#[cfg(test)]
mod tests {
    #[test]
    fn generated_sdk_matches_rust_api() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sdk/src/generated.ts");
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            super::typescript(),
            "Regenerate bindings with cargo run -- --generate-plugin-sdk"
        );
    }
}
