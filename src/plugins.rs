//! Embedded Boa runtimes, one worker thread per plugin. UI mutations stay on the UI thread.
use crate::{
    config::Plugin,
    plugin_api::{API_VERSION, Element, Event, Operation, Panel, PanelPlacement, PluginCommand},
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
    collections::{BTreeMap, VecDeque},
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

struct Bridge {
    requests: SyncSender<Result<Pending, String>>,
    repaint: egui::Context,
    stopped: Arc<AtomicBool>,
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

fn run(source: String, events: Receiver<Event>, bridge: Bridge) -> Result<()> {
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
    while let Ok(event) = events.recv() {
        if ctx
            .get_data::<Bridge>()
            .unwrap()
            .stopped
            .load(Ordering::Relaxed)
        {
            break;
        }
        execute(
            &mut ctx,
            &format!("__slerm_dispatch({});", serde_json::to_string(&event)?),
        )?;
    }
    Ok(())
}

pub struct Runtime {
    pub id: String,
    pub commands: BTreeMap<String, PluginCommand>,
    pub panels: BTreeMap<String, Panel>,
    pub last_state: Option<Value>,
    outgoing: Option<SyncSender<Event>>,
    incoming: Option<Receiver<Result<Pending, String>>>,
    stopped: Arc<AtomicBool>,
}

impl Runtime {
    pub fn spawn(config: &Plugin, config_dir: &Path, ctx: &egui::Context) -> Result<Self> {
        let path = config_dir.join(&config.path);
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("Cannot read plugin {} at {}", config.id, path.display()))?;
        ensure!(
            source.len() <= 4 * MAX_MESSAGE,
            "plugin bundle exceeds 4 MiB"
        );
        let (outgoing, events) = mpsc::sync_channel(64);
        let (requests, incoming) = mpsc::sync_channel(64);
        let stopped = Arc::new(AtomicBool::new(false));
        let bridge = Bridge {
            requests: requests.clone(),
            repaint: ctx.clone(),
            stopped: stopped.clone(),
        };
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
            .try_send(event)
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
        ensure!(
            self.elements.len() <= 128,
            "a panel supports at most 128 elements"
        );
        let mut ids = std::collections::HashSet::new();
        let mut total_rows = 0;
        for element in &self.elements {
            if let Element::Tree { rows, selected, .. } = element {
                total_rows += rows.len();
                ensure!(
                    total_rows <= 8192,
                    "a panel supports at most 8192 tree rows"
                );
                let mut row_ids = std::collections::HashSet::new();
                for (index, row) in rows.iter().enumerate() {
                    ensure!(
                        !row.id.is_empty() && row.id.len() <= 4096 && row_ids.insert(&row.id),
                        "invalid or duplicate tree row id"
                    );
                    ensure!(
                        row.label.len() <= 4096 && row.depth <= 32,
                        "tree label or depth limit exceeded"
                    );
                    ensure!(
                        !row.expanded || row.expandable,
                        "only expandable rows can be expanded"
                    );
                    if index == 0 {
                        ensure!(row.depth == 0, "first tree row must have depth zero");
                    } else {
                        let previous = &rows[index - 1];
                        ensure!(row.depth <= previous.depth + 1, "tree depth skips a level");
                        ensure!(
                            row.depth <= previous.depth || previous.expanded,
                            "children require an expanded parent"
                        );
                    }
                }
                ensure!(
                    selected.as_ref().is_none_or(|id| row_ids.contains(id)),
                    "selected tree row does not exist"
                );
            }
            let id = match element {
                Element::Button { id, .. }
                | Element::Input { id, .. }
                | Element::Tree { id, .. } => Some(id),
                _ => None,
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
        ctx: &egui::Context,
    ) -> (Self, Vec<String>) {
        let mut host = Self::default();
        let mut errors = Vec::new();
        for config in configs {
            match Runtime::spawn(config, config_dir, ctx) {
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
                                ui.heading(&panel.title);
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
    fn boa_reports_script_errors_and_limits_runaway_loops_and_promises() {
        for source in [
            "throw Error('broken plugin')",
            "while (true) {}",
            "function again() { Promise.resolve().then(again); } again();",
            "__slerm.activate({}, 999)",
            "process.exit(0)",
        ] {
            let (_, events) = mpsc::sync_channel(1);
            let (requests, _) = mpsc::sync_channel(1);
            let bridge = Bridge {
                requests,
                repaint: egui::Context::default(),
                stopped: Arc::new(AtomicBool::new(false)),
            };
            assert!(
                run(source.into(), events, bridge).is_err(),
                "source should fail: {source}"
            );
        }
    }
}
