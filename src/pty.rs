use crate::config::Config;
use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    thread,
};

pub struct Pty {
    pub output: Receiver<Vec<u8>>,
    pub input: SyncSender<Vec<u8>>,
    pub errors: Receiver<String>,
    pub exited: Receiver<()>,
    master: Box<dyn MasterPty + Send>,
    shutdown: Sender<()>,
    reaper: Option<thread::JoinHandle<()>>,
}

impl Pty {
    pub fn spawn(config: &Config, ctx: &egui::Context, cwd: Option<&Path>) -> Result<Self> {
        let pair = native_pty_system().openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 640,
            pixel_height: 384,
        })?;
        let mut cmd = CommandBuilder::new(&config.shell[0]);
        cmd.args(&config.shell[1..]);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "slerm");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        if let Some(dir) = cwd.or(config.working_directory.as_deref()) {
            cmd.cwd(dir);
        }
        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .context("Cannot spawn shell")?;
        drop(pair.slave);
        let (output_tx, output) = mpsc::sync_channel(64);
        let (input, input_rx) = mpsc::sync_channel::<Vec<u8>>(64);
        let (error_tx, errors) = mpsc::channel();
        let (exit_tx, exited) = mpsc::channel();
        let (shutdown, shutdown_rx) = mpsc::channel();
        let read_ctx = ctx.clone();
        let write_ctx = ctx.clone();
        let exit_ctx = ctx.clone();
        let read_errors = error_tx.clone();
        thread::spawn(move || {
            let mut buf = [0; 16384];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if output_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                        read_ctx.request_repaint();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    // Linux PTYs report EIO when the slave closes.
                    Err(e) if e.raw_os_error() == Some(5) => break,
                    Err(e) => {
                        let _ = read_errors.send(format!("PTY read: {e}"));
                        read_ctx.request_repaint();
                        break;
                    }
                }
            }
        });
        thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if let Err(e) = writer.write_all(&bytes).and_then(|_| writer.flush()) {
                    let _ = error_tx.send(format!("PTY write: {e}"));
                    write_ctx.request_repaint();
                    break;
                }
            }
        });
        let reaper = thread::spawn(move || {
            loop {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                if !matches!(
                    shutdown_rx.recv_timeout(std::time::Duration::from_millis(25)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    // The worker owns the unreaped child, so shutdown cannot signal a reused PID.
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
            let _ = exit_tx.send(());
            exit_ctx.request_repaint();
        });
        Ok(Self {
            output,
            input,
            errors,
            exited,
            master: pair.master,
            shutdown,
            reaper: Some(reaper),
        })
    }

    /// The working directory of the foreground process group. Shells that emit
    /// OSC 7 are reported by the terminal instead; this covers the rest.
    pub fn cwd(&self) -> Option<PathBuf> {
        process_cwd(self.master.process_group_leader()?)
    }

    pub fn resize(&self, cols: u16, rows: u16, cw: u32, ch: u32) -> Result<()> {
        self.master.resize(PtySize {
            cols,
            rows,
            pixel_width: (cols as u32 * cw).min(u16::MAX as u32) as u16,
            pixel_height: (rows as u32 * ch).min(u16::MAX as u32) as u16,
        })?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn process_cwd(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// macOS has no /proc, so the current directory comes from `proc_pidinfo`.
#[cfg(target_os = "macos")]
fn process_cwd(pid: i32) -> Option<PathBuf> {
    use std::{ffi::CStr, os::unix::ffi::OsStrExt};

    // <sys/proc_info.h>. `vinfo_stat` is spelled as its 136 bytes of padding
    // because only the path that follows it is of interest here.
    #[repr(C)]
    struct VnodeInfo {
        stat: [u64; 17],
        vtype: i32,
        pad: i32,
        fsid: [i32; 2],
    }
    #[repr(C)]
    struct VnodeInfoPath {
        info: VnodeInfo,
        path: [std::ffi::c_char; 1024],
    }
    #[repr(C)]
    struct VnodePathInfo {
        cdir: VnodeInfoPath,
        rdir: VnodeInfoPath,
    }
    const PROC_PIDVNODEPATHINFO: i32 = 9;

    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut std::ffi::c_void,
            buffersize: i32,
        ) -> i32;
    }

    let size = size_of::<VnodePathInfo>();
    // SAFETY: the call writes at most `size` bytes into `info`, and reports how
    // many it wrote. A short write leaves the path unset, so it is rejected.
    unsafe {
        let mut info = std::mem::zeroed::<VnodePathInfo>();
        let written = proc_pidinfo(
            pid,
            PROC_PIDVNODEPATHINFO,
            0,
            (&raw mut info).cast(),
            size as i32,
        );
        if written < size as i32 {
            return None;
        }
        let path = CStr::from_ptr(info.cdir.path.as_ptr());
        (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_cwd(_pid: i32) -> Option<PathBuf> {
    None
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(reaper) = self.reaper.take() {
            let _ = reaper.join();
        }
    }
}
