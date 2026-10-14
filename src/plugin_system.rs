//! Processes, HTTP, and environment for plugins. Slow calls run on their own threads.
use crate::plugin_api::{FetchResponse, RunOutput, SystemOperation};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const MAX_OUTPUT: u64 = 8 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TIMEOUT: Duration = Duration::from_secs(600);

pub enum Call {
    Now(Value),
    /// Work to run off the plugin thread; its result resolves the promise.
    Later(Box<dyn FnOnce() -> Result<Value> + Send>),
}

/// Validate a call. Mistakes reject immediately, before any thread starts.
/// `env` is the configured `[env]` table, which overrides Slerm's environment.
pub fn call(operation: SystemOperation, env: &Arc<BTreeMap<String, String>>) -> Result<Call> {
    Ok(match operation {
        SystemOperation::Env { name } => {
            Call::Now(json!(env.get(&name).cloned().or(std::env::var(name).ok())))
        }
        SystemOperation::Run {
            program,
            args,
            cwd,
            stdin,
            timeout,
        } => {
            let run = Run {
                program,
                args: args.unwrap_or_default(),
                cwd,
                stdin,
                timeout,
                env: env.clone(),
            };
            let timeout = run.check()?;
            Call::Later(Box::new(move || Ok(json!(run.execute(timeout)?))))
        }
        SystemOperation::Fetch {
            url,
            method,
            headers,
            body,
            timeout,
        } => {
            ensure!(
                url.starts_with("http://") || url.starts_with("https://"),
                "fetch supports http and https URLs"
            );
            let request = ureq::http::Request::builder()
                .method(method.as_deref().unwrap_or("GET"))
                .uri(&url);
            let request = headers
                .unwrap_or_default()
                .iter()
                .fold(request, |request, (name, value)| {
                    request.header(name, value)
                })
                .body(body.unwrap_or_default().into_bytes())
                .context("Invalid request")?;
            let timeout = duration(timeout)?;
            Call::Later(Box::new(move || Ok(json!(fetch(request, timeout)?))))
        }
    })
}

fn duration(ms: Option<f64>) -> Result<Duration> {
    let Some(ms) = ms else {
        return Ok(DEFAULT_TIMEOUT);
    };
    ensure!(ms.is_finite() && ms > 0.0, "timeout must be positive");
    Ok(Duration::from_secs_f64(ms / 1000.0).min(MAX_TIMEOUT))
}

fn fetch(request: ureq::http::Request<Vec<u8>>, timeout: Duration) -> Result<FetchResponse> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into();
    let response = agent.run(request)?;
    let mut headers = BTreeMap::<String, String>::new();
    for (name, value) in response.headers() {
        let Ok(value) = value.to_str() else { continue };
        headers
            .entry(name.as_str().into())
            .and_modify(|joined| {
                joined.push_str(", ");
                joined.push_str(value);
            })
            .or_insert_with(|| value.into());
    }
    let status = response.status().as_u16();
    let body = response
        .into_body()
        .with_config()
        .limit(MAX_OUTPUT)
        .read_to_vec()?;
    Ok(FetchResponse {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

struct Run {
    program: String,
    args: Vec<String>,
    cwd: String,
    stdin: Option<String>,
    timeout: Option<f64>,
    env: Arc<BTreeMap<String, String>>,
}

impl Run {
    /// Validate before spawning a thread, so mistakes reject the call immediately.
    fn check(&self) -> Result<Duration> {
        ensure!(
            !self.program.is_empty() && !self.program.contains('\0'),
            "invalid program"
        );
        ensure!(
            self.args.len() <= 1024 && self.args.iter().all(|arg| !arg.contains('\0')),
            "invalid program arguments"
        );
        let cwd = Path::new(&self.cwd);
        ensure!(
            cwd.is_absolute() && cwd.is_dir(),
            "working directory must be an existing absolute directory"
        );
        duration(self.timeout)
    }

    /// Run to completion, or kill the process when the timeout expires.
    fn execute(self, timeout: Duration) -> Result<RunOutput> {
        // A configured PATH also applies when looking up the program.
        let mut child = Command::new(&self.program)
            .envs(self.env.iter())
            .args(&self.args)
            .current_dir(&self.cwd)
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Cannot run {}", self.program))?;
        // Feed and drain pipes on their own threads so a chatty process cannot block.
        let stdin = child.stdin.take().zip(self.stdin).map(|(mut pipe, text)| {
            thread::spawn(move || {
                let _ = pipe.write_all(text.as_bytes());
            })
        });
        let read = |pipe: Option<Box<dyn Read + Send>>| {
            thread::spawn(move || {
                let mut out = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = (&mut pipe).take(MAX_OUTPUT).read_to_end(&mut out);
                    let _ = std::io::copy(&mut pipe, &mut std::io::sink());
                }
                String::from_utf8_lossy(&out).into_owned()
            })
        };
        let stdout = read(child.stdout.take().map(|p| Box::new(p) as _));
        let stderr = read(child.stderr.take().map(|p| Box::new(p) as _));
        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                let _ = child.kill();
                break child.wait()?;
            }
            thread::sleep(Duration::from_millis(5));
        };
        if let Some(stdin) = stdin {
            let _ = stdin.join();
        }
        Ok(RunOutput {
            status: status.code(),
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
            timed_out,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str], stdin: Option<&str>, timeout: Option<f64>) -> Result<RunOutput> {
        let run = Run {
            program: "/bin/sh".into(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            cwd: "/".into(),
            stdin: stdin.map(Into::into),
            timeout,
            env: Arc::new(BTreeMap::from([("SLERM_TEST".into(), "configured".into())])),
        };
        let timeout = run.check()?;
        run.execute(timeout)
    }

    #[test]
    fn captures_output_status_stdin_and_timeouts() {
        let output = run(&["-c", "pwd; echo oops >&2; exit 3"], None, None).unwrap();
        assert_eq!(
            (
                output.status,
                output.stdout.as_str(),
                output.stderr.as_str()
            ),
            (Some(3), "/\n", "oops\n")
        );
        assert_eq!(
            run(&["-c", "cat"], Some("piped"), None).unwrap().stdout,
            "piped"
        );
        let output = run(&["-c", "printf %s \"$SLERM_TEST\""], None, None).unwrap();
        assert_eq!(output.stdout, "configured");
        let started = Instant::now();
        let output = run(&["-c", "sleep 5"], None, Some(50.0)).unwrap();
        assert!(output.timed_out && output.status.is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(run(&[], None, Some(-1.0)).is_err());
        let relative = Run {
            program: "/bin/sh".into(),
            args: vec![],
            cwd: "tmp".into(),
            stdin: None,
            timeout: None,
            env: Default::default(),
        };
        assert!(relative.check().is_err());
    }

    fn later(operation: Value) -> Result<Value> {
        match call(serde_json::from_value(operation)?, &Default::default())? {
            Call::Later(work) => work(),
            Call::Now(_) => unreachable!(),
        }
    }

    #[test]
    fn fetches_over_http_and_reports_any_status() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            // Read the head and the 5-byte body.
            while !String::from_utf8_lossy(&request).contains("\r\n\r\nhello") {
                let read = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(b"HTTP/1.1 418 I'm a teapot\r\nX-Tea: green\r\nX-Tea: black\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
            String::from_utf8(request).unwrap()
        });
        let response = later(json!({"method": "fetch", "params": {"url": format!("http://127.0.0.1:{port}/brew"), "method": "POST",
            "headers": {"x-sender": "slerm"}, "body": "hello", "timeout": 5000}}))
        .unwrap();
        assert_eq!(response["status"], 418);
        assert_eq!(response["body"], "ok");
        assert_eq!(response["headers"]["x-tea"], "green, black");
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /brew"), "{request}");
        assert!(request.to_lowercase().contains("x-sender: slerm"));
        assert!(later(json!({"method": "fetch", "params": {"url": "file:///etc/hosts"}})).is_err());
        let env = |name| {
            let configured = Arc::new(BTreeMap::from([("PATH".into(), "/configured".into())]));
            let operation = json!({"method": "env", "params": {"name": name}});
            match call(serde_json::from_value(operation).unwrap(), &configured).unwrap() {
                Call::Now(value) => value,
                Call::Later(_) => unreachable!(),
            }
        };
        assert_eq!(env("PATH"), "/configured");
        assert!(env("HOME").is_string());
        assert!(env("SLERM_SURELY_UNSET").is_null());
    }
}
