//! The source of truth for the plugin API and its generated TypeScript bindings.
use crate::config::Action;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const API_VERSION: u32 = 1;

// Declare each method, its parameters and its result once. The enum drives Rust
// dispatch; the generated result map drives the TypeScript client's signatures.
macro_rules! plugin_api {
    ($operation:ident, $results:ident; $($variant:ident => $method:ident { $($(#[$attr:meta])* $arg:ident: $ty:ty),* } -> $result:ty;)+) => {
        #[derive(Debug, Deserialize, TS)]
        #[serde(tag = "method", rename_all = "camelCase", deny_unknown_fields)]
        pub enum $operation {
            $($variant { $($(#[$attr])* $arg: $ty),* },)+
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
    /// Expansion and selection belong to the host and survive panel updates.
    #[serde(skip)]
    #[ts(skip)]
    pub trees: std::collections::HashMap<String, crate::plugin_ui::TreeState>,
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

#[derive(Clone, Debug, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Element {
    Text {
        text: String,
    },
    Button {
        id: String,
        label: String,
    },
    Input {
        id: String,
        label: String,
        value: String,
    },
    Separator,
    /// Lays out its elements horizontally.
    Row {
        elements: Vec<Element>,
    },
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
        PluginCommand::decl(&config),
        Panel::decl(&config),
        PanelPlacement::decl(&config),
        TreeNode::decl(&config),
        Element::decl(&config),
        UIEvent::decl(&config),
        Event::decl(&config),
    ] {
        output.push_str("export ");
        output.push_str(&declaration);
        output.push_str("\n\n");
    }
    output.push_str(r#"type Params<O, M> = Omit<Extract<O, { method: M }>, "method">;
type Client<O extends { method: string }, R extends Record<O["method"], unknown>> = {
  [M in O["method"]]: (...args: keyof Params<O, M> extends never ? [] : [params: Params<O, M>]) => Promise<R[M] extends null ? void : R[M]>;
};
export type Slerm = Client<Operation, Results>;
export type Fs = Client<FsOperation, FsResults>;
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
