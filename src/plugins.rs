//! Embedded Boa runtimes, one worker thread per plugin. UI mutations stay on the UI thread.
use crate::{
    config::Plugin,
    plugin_api::{
        API_VERSION, Element, Event, Operation, Panel, PanelPlacement, PluginCommand, TreeNode,
    },
};
use anyhow::{Context as _, Result, ensure};
use boa_engine::{
    Context, JsNativeError, JsResult, JsValue, NativeFunction, Source,
    job::{Job, JobExecutor},
    js_string,
    property::Attribute,
};
use serde_json::Value;
use std::{
    cell::RefCell,
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap, VecDeque},
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

const MAX_MESSAGE: usize = 1024 * 1024;

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}

pub struct Pending {
    pub operation: Operation,
    pub reply: SyncSender<Value>,
}

/// Concurrent `run` and `fetch` calls per plugin.
const MAX_RUNNING: usize = 16;
const MAX_TIMERS: usize = 4096;

/// What the worker thread waits for: UI events, and slow calls that finished.
pub enum Message {
    Event(Event),
    Done { id: u64, reply: Value },
}

struct Bridge {
    requests: SyncSender<Result<Pending, String>>,
    repaint: egui::Context,
    stopped: Arc<AtomicBool>,
    inbox: SyncSender<Message>,
    env: Arc<BTreeMap<String, String>>,
    next_call: std::cell::Cell<u64>,
    running: std::cell::Cell<usize>,
    /// Pending timers by deadline, holding the JavaScript timer handle.
    timers: RefCell<BinaryHeap<Reverse<(Instant, u64)>>>,
}

impl Bridge {
    fn new(
        requests: SyncSender<Result<Pending, String>>,
        repaint: egui::Context,
        stopped: Arc<AtomicBool>,
        inbox: SyncSender<Message>,
        env: Arc<BTreeMap<String, String>>,
    ) -> Self {
        Self {
            requests,
            repaint,
            stopped,
            inbox,
            env,
            next_call: Default::default(),
            running: Default::default(),
            timers: Default::default(),
        }
    }
}

fn request<T: serde::de::DeserializeOwned>(args: &[JsValue], ctx: &mut Context) -> JsResult<T> {
    let text = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_string(ctx)?
        .to_std_string_escaped();
    if text.len() > MAX_MESSAGE {
        return Err(JsNativeError::range()
            .with_message("plugin request exceeds 1 MiB")
            .into());
    }
    serde_json::from_str(&text).map_err(|e| JsNativeError::typ().with_message(e.to_string()).into())
}

fn host_call(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let operation: Operation = request(args, ctx)?;
    let bridge = ctx.get_data::<Bridge>().unwrap();
    let (reply, response) = mpsc::sync_channel(1);
    bridge
        .requests
        .try_send(Ok(Pending { operation, reply }))
        .map_err(|_| JsNativeError::error().with_message("plugin host is unavailable"))?;
    bridge.repaint.request_repaint();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if bridge.stopped.load(Ordering::Relaxed) || Instant::now() >= deadline {
            return Err(JsNativeError::error()
                .with_message("plugin stopped or host request timed out")
                .into());
        }
        match response.recv_timeout(Duration::from_millis(50)) {
            Ok(value) => return Ok(js_string!(serde_json::to_string(&value).unwrap()).into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => {
                return Err(JsNativeError::error()
                    .with_message("plugin host disconnected")
                    .into());
            }
        }
    }
}

// Filesystem calls block only this plugin's worker, never the UI thread.
fn fs_call(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let reply = match crate::plugin_fs::call(request(args, ctx)?) {
        Ok(result) => serde_json::json!({"result": result}),
        Err(error) => serde_json::json!({"error": format!("{error:#}")}),
    };
    Ok(js_string!(serde_json::to_string(&reply).unwrap()).into())
}

fn reply(reply: Value) -> JsValue {
    js_string!(serde_json::to_string(&reply).unwrap()).into()
}

// Fast calls answer at once; slow ones answer {"pending": id} and finish later
// through `__slerm_complete`, without blocking this plugin's thread.
fn system_call(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    use crate::plugin_system::Call;
    let operation = request(args, ctx)?;
    let env = ctx.get_data::<Bridge>().unwrap().env.clone();
    let call = match crate::plugin_system::call(operation, &env) {
        Ok(call) => call,
        Err(error) => return Ok(reply(serde_json::json!({"error": format!("{error:#}")}))),
    };
    let work = match call {
        Call::Now(result) => return Ok(reply(serde_json::json!({"result": result}))),
        Call::Later(work) => work,
    };
    let bridge = ctx.get_data::<Bridge>().unwrap();
    if bridge.running.get() >= MAX_RUNNING {
        return Ok(reply(serde_json::json!({
            "error": format!("at most {MAX_RUNNING} calls can run at once")
        })));
    }
    let id = bridge.next_call.get();
    bridge.next_call.set(id + 1);
    bridge.running.set(bridge.running.get() + 1);
    let inbox = bridge.inbox.clone();
    thread::spawn(move || {
        let reply = match work() {
            Ok(result) => serde_json::json!({"result": result}),
            Err(error) => serde_json::json!({"error": format!("{error:#}")}),
        };
        // The plugin may have stopped; then nobody is waiting.
        let _ = inbox.send(Message::Done { id, reply });
    });
    Ok(reply(serde_json::json!({"pending": id})))
}

/// `__slerm_timer(handle, ms)` schedules `__slerm_fire(handle)`.
fn timer(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let handle = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_number(ctx)?;
    let ms = args
        .get(1)
        .unwrap_or(&JsValue::undefined())
        .to_number(ctx)?;
    let bridge = ctx.get_data::<Bridge>().unwrap();
    let mut timers = bridge.timers.borrow_mut();
    if timers.len() >= MAX_TIMERS {
        return Err(JsNativeError::range()
            .with_message(format!("at most {MAX_TIMERS} timers can be pending"))
            .into());
    }
    let delay = Duration::from_secs_f64(ms.clamp(0.0, 2_147_483_647.0) / 1000.0);
    timers.push(Reverse((Instant::now() + delay, handle as u64)));
    Ok(JsValue::undefined())
}

fn host_error(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let text = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_string(ctx)?
        .to_std_string_escaped();
    let bridge = ctx.get_data::<Bridge>().unwrap();
    let _ = bridge.requests.try_send(Err(text));
    bridge.repaint.request_repaint();
    Ok(JsValue::undefined())
}

// This runtime exposes promises, but no timers or native async jobs. Bound each
// promise checkpoint as well as JS loops, so runaway callbacks can be stopped.
struct Jobs {
    queue: RefCell<VecDeque<Job>>,
    stopped: Arc<AtomicBool>,
}
impl JobExecutor for Jobs {
    fn enqueue_job(self: Rc<Self>, job: Job, _: &mut Context) {
        self.queue.borrow_mut().push_back(job);
    }
    fn run_jobs(self: Rc<Self>, ctx: &mut Context) -> JsResult<()> {
        for _ in 0..10_000 {
            if self.stopped.load(Ordering::Relaxed) {
                return Err(JsNativeError::error().with_message("plugin stopped").into());
            }
            let Some(job) = self.queue.borrow_mut().pop_front() else {
                return Ok(());
            };
            match job {
                Job::PromiseJob(job) => {
                    job.call(ctx)?;
                }
                Job::GenericJob(job) => {
                    job.call(ctx)?;
                }
                Job::FinalizationRegistryCleanupJob(_) => {}
                _ => {
                    return Err(JsNativeError::error()
                        .with_message("unsupported plugin job")
                        .into());
                }
            }
        }
        Err(JsNativeError::error()
            .with_message("plugin promise job limit exceeded")
            .into())
    }
}

fn run(source: String, events: Receiver<Message>, bridge: Bridge) -> Result<()> {
    let jobs = Rc::new(Jobs {
        queue: RefCell::new(VecDeque::new()),
        stopped: bridge.stopped.clone(),
    });
    let mut ctx = Context::builder()
        .job_executor(jobs)
        .build()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    ctx.runtime_limits_mut().set_loop_iteration_limit(1_000_000);
    ctx.runtime_limits_mut().set_recursion_limit(128);
    ctx.insert_data(bridge);
    let execute = |ctx: &mut Context, source: &str| -> Result<()> {
        ctx.eval(Source::from_bytes(source))
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        ctx.run_jobs().map_err(|e| anyhow::anyhow!(e.to_string()))?;
        Ok(())
    };
    ctx.register_global_builtin_callable(
        js_string!("__slerm_call"),
        1,
        NativeFunction::from_fn_ptr(host_call),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    ctx.register_global_builtin_callable(
        js_string!("__slerm_fs"),
        1,
        NativeFunction::from_fn_ptr(fs_call),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    ctx.register_global_builtin_callable(
        js_string!("__slerm_system"),
        1,
        NativeFunction::from_fn_ptr(system_call),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    ctx.register_global_builtin_callable(
        js_string!("__slerm_timer"),
        2,
        NativeFunction::from_fn_ptr(timer),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    ctx.register_global_builtin_callable(
        js_string!("__slerm_error"),
        1,
        NativeFunction::from_fn_ptr(host_error),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    ctx.register_global_property(
        js_string!("__slerm_version"),
        API_VERSION,
        Attribute::READONLY,
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    execute(&mut ctx, include_str!("../sdk/runtime.js"))?;
    execute(&mut ctx, &source)?;
    loop {
        let bridge = ctx.get_data::<Bridge>().unwrap();
        if bridge.stopped.load(Ordering::Relaxed) {
            break;
        }
        let now = Instant::now();
        let mut due = Vec::new();
        let next = {
            let mut timers = bridge.timers.borrow_mut();
            while let Some(Reverse((deadline, handle))) = timers.peek().copied()
                && deadline <= now
            {
                timers.pop();
                due.push(handle);
            }
            timers.peek().map(|Reverse((deadline, _))| *deadline)
        };
        for handle in due {
            execute(&mut ctx, &format!("__slerm_fire({handle});"))?;
        }
        // Wake for the next timer, and at least every 250 ms to notice a stop.
        let wait = next
            .map_or(Duration::MAX, |deadline| {
                deadline.saturating_duration_since(now)
            })
            .min(Duration::from_millis(250));
        match events.recv_timeout(wait) {
            Ok(Message::Event(event)) => execute(
                &mut ctx,
                &format!("__slerm_dispatch({});", serde_json::to_string(&event)?),
            )?,
            Ok(Message::Done { id, reply }) => {
                let bridge = ctx.get_data::<Bridge>().unwrap();
                bridge.running.set(bridge.running.get() - 1);
                execute(
                    &mut ctx,
                    &format!(
                        "__slerm_complete({id}, {});",
                        serde_json::to_string(&reply)?
                    ),
                )?
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

pub struct Runtime {
    pub id: String,
    pub commands: BTreeMap<String, PluginCommand>,
    pub panels: BTreeMap<String, Panel>,
    pub last_state: Option<Value>,
    outgoing: Option<SyncSender<Message>>,
    incoming: Option<Receiver<Result<Pending, String>>>,
    stopped: Arc<AtomicBool>,
}

impl Runtime {
    pub fn spawn(
        config: &Plugin,
        config_dir: &Path,
        env: Arc<BTreeMap<String, String>>,
        ctx: &egui::Context,
    ) -> Result<Self> {
        let path = config_dir.join(&config.path);
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("Cannot read plugin {} at {}", config.id, path.display()))?;
        ensure!(
            source.len() <= 4 * MAX_MESSAGE,
            "plugin bundle exceeds 4 MiB"
        );
        // Leave room for every running call to report back while the queue
        // is full of UI events.
        let (outgoing, events) = mpsc::sync_channel(64 + MAX_RUNNING);
        let (requests, incoming) = mpsc::sync_channel(64);
        let stopped = Arc::new(AtomicBool::new(false));
        let bridge = Bridge::new(
            requests.clone(),
            ctx.clone(),
            stopped.clone(),
            outgoing.clone(),
            env,
        );
        let repaint = ctx.clone();
        thread::Builder::new()
            .name(format!("plugin-{}", config.id))
            .spawn(move || {
                if let Err(error) = run(source, events, bridge) {
                    let _ = requests.try_send(Err(error.to_string()));
                    repaint.request_repaint();
                }
            })?;
        Ok(Self {
            id: config.id.clone(),
            commands: BTreeMap::new(),
            panels: BTreeMap::new(),
            last_state: None,
            outgoing: Some(outgoing),
            incoming: Some(incoming),
            stopped,
        })
    }

    pub fn poll(&mut self) -> Option<Result<Pending, String>> {
        match self.incoming.as_ref()?.try_recv() {
            Ok(request) => Some(request),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("plugin runtime stopped".into())),
        }
    }

    pub fn send(&self, event: Event) -> Result<()> {
        let outgoing = self
            .outgoing
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("plugin is stopped"))?;
        ensure!(
            serde_json::to_vec(&event)?.len() < MAX_MESSAGE,
            "plugin event exceeds 1 MiB"
        );
        outgoing
            .try_send(Message::Event(event))
            .context("plugin event queue is full or disconnected")
    }

    pub fn is_running(&self) -> bool {
        !self.stopped.load(Ordering::Relaxed)
    }

    pub fn stop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.commands.clear();
        self.panels.clear();
        self.outgoing.take();
        self.incoming.take();
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Panel {
    pub fn validate(&self) -> Result<()> {
        ensure!(valid_id(&self.id), "invalid panel id");
        ensure!(
            !self.title.trim().is_empty() && self.title.len() <= 200,
            "invalid panel title"
        );
        let keys = self.keys.as_deref().unwrap_or_default();
        ensure!(keys.len() <= 64, "a panel declares at most 64 keys");
        for key in keys {
            crate::config::shortcut(key).with_context(|| format!("panel key {key}"))?;
        }
        let mut ids = std::collections::HashSet::new();
        let mut count = 0;
        validate_elements(&self.elements, 0, &mut count, &mut ids)?;
        let mut nodes = 0;
        let mut result = Ok(());
        crate::plugin_ui::visit_trees(&self.elements, &mut |_, tree| {
            let mut node_ids = std::collections::HashSet::new();
            if result.is_ok() {
                result = validate_nodes(tree, 0, &mut nodes, &mut node_ids);
            }
        });
        result
    }
}

fn validate_elements<'a>(
    elements: &'a [Element],
    depth: usize,
    count: &mut usize,
    ids: &mut std::collections::HashSet<&'a str>,
) -> Result<()> {
    ensure!(depth <= 4, "rows and sections nest at most 4 deep");
    for element in elements {
        *count += 1;
        ensure!(*count <= 128, "a panel supports at most 128 elements");
        let id = match element {
            Element::Row { elements } => {
                validate_elements(elements, depth + 1, count, ids)?;
                None
            }
            Element::Section { id, elements, .. } => {
                validate_elements(elements, depth + 1, count, ids)?;
                Some(id)
            }
            Element::TextArea { id, rows, .. } => {
                ensure!(
                    rows.is_none_or(|rows| (1..=50).contains(&rows)),
                    "text areas show 1 to 50 rows"
                );
                Some(id)
            }
            Element::Slider {
                id,
                value,
                min,
                max,
                step,
                ..
            } => {
                ensure!(
                    value.is_finite() && min.is_finite() && max.is_finite() && min < max,
                    "sliders need finite values and min < max"
                );
                ensure!(
                    step.is_none_or(|step| step.is_finite() && step > 0.0),
                    "slider steps must be positive"
                );
                Some(id)
            }
            Element::Select { id, options, .. } => {
                ensure!(options.len() <= 256, "a select has at most 256 options");
                Some(id)
            }
            Element::Progress { value, .. } => {
                ensure!(value.is_finite(), "progress must be finite");
                None
            }
            Element::Table {
                id, columns, rows, ..
            } => {
                ensure!(
                    (1..=16).contains(&columns.len()),
                    "tables have 1 to 16 columns"
                );
                ensure!(rows.len() <= 4096, "a table has at most 4096 rows");
                let mut row_ids = std::collections::HashSet::new();
                for row in rows {
                    ensure!(
                        !row.id.is_empty() && row.id.len() <= 4096 && row_ids.insert(&row.id),
                        "invalid or duplicate table row id"
                    );
                    ensure!(
                        row.cells.len() == columns.len(),
                        "each table row needs one cell per column"
                    );
                }
                Some(id)
            }
            Element::Button { id, .. }
            | Element::Input { id, .. }
            | Element::Checkbox { id, .. }
            | Element::Tree { id, .. } => Some(id),
            Element::Text { .. } | Element::Separator | Element::Spinner => None,
        };
        if let Some(id) = id {
            ensure!(
                valid_id(id) && ids.insert(id),
                "invalid or duplicate control id"
            );
        }
    }
    Ok(())
}

fn validate_nodes<'a>(
    nodes: &'a [TreeNode],
    depth: usize,
    count: &mut usize,
    ids: &mut std::collections::HashSet<&'a str>,
) -> Result<()> {
    ensure!(depth < 64, "tree nodes nest at most 64 deep");
    for node in nodes {
        *count += 1;
        ensure!(
            *count <= 16_384,
            "a panel supports at most 16384 tree nodes"
        );
        ensure!(
            !node.id.is_empty() && node.id.len() <= 4096 && ids.insert(&node.id),
            "invalid or duplicate tree node id"
        );
        ensure!(node.label.len() <= 4096, "tree label exceeds 4096 bytes");
        ensure!(
            node.icon
                .as_ref()
                .is_none_or(|icon| icon.chars().count() <= 4),
            "tree icons are at most 4 characters"
        );
        if let Some(children) = &node.children {
            validate_nodes(children, depth + 1, count, ids)?;
        }
    }
    Ok(())
}

#[derive(Default)]
pub struct Host {
    pub runtimes: Vec<Runtime>,
    pub next_state_poll: Option<Instant>,
}

impl Host {
    pub fn start(
        configs: &[Plugin],
        config_dir: &Path,
        env: &BTreeMap<String, String>,
        ctx: &egui::Context,
    ) -> (Self, Vec<String>) {
        let mut host = Self::default();
        let mut errors = Vec::new();
        let env = Arc::new(env.clone());
        for config in configs {
            match Runtime::spawn(config, config_dir, env.clone(), ctx) {
                Ok(process) => host.runtimes.push(process),
                Err(e) => errors.push(e.to_string()),
            }
        }
        (host, errors)
    }

    pub fn invoke(&self, index: usize, command: &str) -> Result<()> {
        let plugin = &self.runtimes[index];
        ensure!(
            plugin.commands.contains_key(command),
            "unknown plugin command"
        );
        plugin.send(Event::Command {
            command: command.into(),
        })
    }

    /// Run a command by plugin ID, as a key binding does.
    pub fn invoke_named(&self, plugin: &str, command: &str) -> Result<()> {
        let index = self
            .runtimes
            .iter()
            .position(|runtime| runtime.id == plugin && runtime.is_running())
            .with_context(|| format!("plugin {plugin} is not running"))?;
        ensure!(
            self.runtimes[index].commands.contains_key(command),
            "plugin {plugin} has no command {command}"
        );
        self.invoke(index, command)
    }

    pub fn show_panels(&mut self, ctx: &egui::Context) -> Vec<String> {
        let mut errors = Vec::new();
        for plugin in &mut self.runtimes {
            let mut events = Vec::new();
            for panel in plugin.panels.values_mut() {
                let id = egui::Id::new(("plugin panel", &plugin.id, &panel.id));
                match panel.placement.unwrap_or(PanelPlacement::Floating) {
                    PanelPlacement::Floating => {
                        egui::Window::new(&panel.title)
                            .id(id)
                            .order(egui::Order::Foreground)
                            .default_size(egui::vec2(280.0, 420.0))
                            .show(ctx, |ui| crate::plugin_ui::show(ui, panel, &mut events));
                    }
                    placement => {
                        let side = match placement {
                            PanelPlacement::Left => egui::SidePanel::left(id),
                            _ => egui::SidePanel::right(id),
                        };
                        side.resizable(true)
                            .default_width(260.0)
                            .width_range(160.0..=600.0)
                            .show(ctx, |ui| {
                                ui.add_space(4.0);
                                ui.label(egui::RichText::new(&panel.title).strong());
                                ui.separator();
                                crate::plugin_ui::show(ui, panel, &mut events);
                            });
                    }
                }
            }
            for event in events {
                if let Err(e) = plugin.send(Event::Ui { data: event }) {
                    errors.push(format!("{}: {e}", plugin.id));
                }
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_invalid_configuration_and_ui() {
        assert!(crate::config::Config::parse("[[plugins]]\nid='x'\npath=''\n").is_err());
        assert!(
            crate::config::Config::parse(
                "[[plugins]]\nid='x'\npath='a.js'\n[[plugins]]\nid='x'\npath='b.js'\n"
            )
            .is_err()
        );
        assert!(serde_json::from_str::<Operation>(r#"{"method":"unknown"}"#).is_err());
        let panel: Panel =
            serde_json::from_value(json!({"id":"panel", "title":"Panel", "elements":[
                {"type":"button", "id":"same", "label":"One"},
                {"type":"input", "id":"same", "label":"Two", "value":""}
            ]}))
            .unwrap();
        assert!(panel.validate().is_err());
    }

    #[test]
    fn validates_nested_rows_and_tree_nodes() {
        let panel = |elements| {
            serde_json::from_value::<Panel>(json!({"id":"p", "title":"P", "elements": elements}))
                .unwrap()
        };
        let node = |id: &str, children| json!({"id": id, "label": id, "children": children});
        assert!(
            panel(json!([
                {"type":"row", "elements":[{"type":"button", "id":"a", "label":"A"}]},
                {"type":"tree", "id":"t", "nodes":[node("x", json!([node("y", json!([]))]))]}
            ]))
            .validate()
            .is_ok()
        );
        // Control IDs are unique across nested rows; node IDs within a tree.
        assert!(
            panel(json!([
                {"type":"row", "elements":[{"type":"button", "id":"a", "label":"A"}]},
                {"type":"button", "id":"a", "label":"B"}
            ]))
            .validate()
            .is_err()
        );
        assert!(
            panel(json!([{"type":"tree", "id":"t", "nodes":[node("x", json!([node("x", json!([]))]))]}]))
                .validate()
                .is_err()
        );
        assert!(
            panel(json!([{"type":"section", "id":"s", "title":"S", "elements":[
                {"type":"slider", "id":"n", "value":1, "min":0, "max":2, "step":0.5},
                {"type":"select", "id":"c", "value":"a", "options":["a", "b"]},
                {"type":"table", "id":"t", "columns":["A", "B"], "rows":[{"id":"r", "cells":["1", "2"]}]},
                {"type":"textArea", "id":"x", "value":"", "rows":4, "code":true},
                {"type":"checkbox", "id":"k", "label":"K", "checked":true},
                {"type":"progress", "value":0.5}, {"type":"spinner"},
                {"type":"text", "text":"T", "style":"heading", "color":"error"}
            ]}]))
            .validate()
            .is_ok()
        );
        for invalid in [
            json!({"type":"slider", "id":"n", "value":1, "min":2, "max":2}),
            json!({"type":"slider", "id":"n", "value":1, "min":0, "max":2, "step":0}),
            json!({"type":"table", "id":"t", "columns":["A", "B"], "rows":[{"id":"r", "cells":["1"]}]}),
            json!({"type":"table", "id":"t", "columns":[], "rows":[]}),
            json!({"type":"textArea", "id":"x", "value":"", "rows":0}),
            json!({"type":"section", "id":"s", "title":"S", "elements":[{"type":"button", "id":"s", "label":"B"}]}),
        ] {
            assert!(panel(json!([invalid])).validate().is_err(), "{invalid}");
        }
        let mut deep = json!([]);
        for depth in 0..70 {
            deep = json!([node(&depth.to_string(), deep)]);
        }
        assert!(
            panel(json!([{"type":"tree", "id":"t", "nodes": deep}]))
                .validate()
                .is_err()
        );
    }

    #[test]
    fn boa_reports_script_errors_and_limits_runaway_loops_and_promises() {
        for source in [
            "throw Error('broken plugin')",
            "while (true) {}",
            "function again() { Promise.resolve().then(again); } again();",
            "__slerm.activate({}, 999)",
            "process.exit(0)",
        ] {
            let (inbox, events) = mpsc::sync_channel(1);
            let (requests, _) = mpsc::sync_channel(1);
            let bridge = Bridge::new(
                requests,
                egui::Context::default(),
                Arc::new(AtomicBool::new(false)),
                inbox,
                Default::default(),
            );
            assert!(
                run(source.into(), events, bridge).is_err(),
                "source should fail: {source}"
            );
        }
    }
}
