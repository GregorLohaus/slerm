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
        for (name, value) in &config.env {
            cmd.env(name, value);
        }
        #[cfg(unix)]
        configure_utf8_locale(&mut cmd);
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

/// GUI launches may have no locale at all. In particular, macOS pbcopy then
/// decodes Helix's UTF-8 bytes as a legacy encoding. Configure the child, not
/// the multithreaded parent, and honor an existing, valid UTF-8 locale.
#[cfg(unix)]
fn configure_utf8_locale(cmd: &mut CommandBuilder) {
    let effective = ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .find_map(|key| cmd.get_env(key).filter(|value| !value.is_empty()));
    if effective.is_some_and(locale_is_utf8) {
        return;
    }
    let candidates = if cfg!(target_os = "macos") {
        ["en_US.UTF-8", "C.UTF-8", "UTF-8"]
    } else {
        ["C.UTF-8", "en_US.UTF-8", "UTF-8"]
    };
    let Some(locale) = candidates
        .into_iter()
        .find(|name| locale_is_utf8(name.as_ref()))
    else {
        return;
    };
    if cmd.get_env("LANG").is_none_or(|value| value.is_empty()) {
        cmd.env("LANG", locale);
    }
    cmd.env("LC_CTYPE", locale);
    // LC_ALL overrides LC_CTYPE, including when it is C, POSIX or invalid.
    if cmd.get_env("LC_ALL").is_some_and(|value| !value.is_empty()) {
        cmd.env("LC_ALL", locale);
    }
}

#[cfg(unix)]
fn locale_is_utf8(name: &std::ffi::OsStr) -> bool {
    use std::{
        ffi::{CStr, CString},
        os::unix::ffi::OsStrExt,
    };

    // libc does not expose this POSIX function on macOS yet.
    unsafe extern "C" {
        fn nl_langinfo_l(item: libc::nl_item, locale: libc::locale_t) -> *mut libc::c_char;
    }

    let Ok(name) = CString::new(name.as_bytes()) else {
        return false;
    };
    // SAFETY: newlocale creates an independent locale object and does not
    // change process-global state. Its codeset string lives until freelocale.
    unsafe {
        let locale = libc::newlocale(libc::LC_CTYPE_MASK, name.as_ptr(), std::ptr::null_mut());
        if locale.is_null() {
            return false;
        }
        let codeset = nl_langinfo_l(libc::CODESET, locale);
        let utf8 =
            !codeset.is_null() && matches!(CStr::from_ptr(codeset).to_bytes(), b"UTF-8" | b"UTF8");
        libc::freelocale(locale);
        utf8
    }
}

#[cfg(all(test, unix))]
mod locale_tests {
    use super::*;

    #[test]
    fn gui_and_non_utf8_environments_get_a_working_utf8_locale() {
        for vars in [
            vec![],
            vec![("LANG", "")],
            vec![("LANG", "C")],
            vec![("LC_ALL", "POSIX"), ("LANG", "en_US.UTF-8")],
            vec![("LC_CTYPE", "C"), ("LANG", "en_US.UTF-8")],
            vec![("LC_ALL", "slerm_missing.UTF-8")],
        ] {
            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.env_clear();
            for (key, value) in vars {
                cmd.env(key, value);
            }
            cmd.env("LC_TIME", "C");
            configure_utf8_locale(&mut cmd);
            let effective = ["LC_ALL", "LC_CTYPE", "LANG"]
                .into_iter()
                .find_map(|key| cmd.get_env(key).filter(|value| !value.is_empty()))
                .unwrap();
            assert!(locale_is_utf8(effective), "{effective:?}");
            assert_eq!(cmd.get_env("LC_TIME"), Some(std::ffi::OsStr::new("C")));
            // Check the same native locale initialization that pbcopy uses,
            // without changing the user's system clipboard in a test.
            #[cfg(target_os = "macos")]
            {
                let output = std::process::Command::new("/usr/bin/locale")
                    .arg("charmap")
                    .env_clear()
                    .envs(cmd.iter_full_env_as_str())
                    .output()
                    .unwrap();
                assert!(output.status.success());
                assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "UTF-8");
            }
        }
        assert!(!locale_is_utf8("slerm_missing.UTF-8".as_ref()));
    }

    #[test]
    fn existing_utf8_locale_and_precedence_are_preserved() {
        let mut base = CommandBuilder::new("/bin/sh");
        base.env_clear();
        configure_utf8_locale(&mut base);
        let utf8 = base.get_env("LC_CTYPE").unwrap().to_owned();
        for key in ["LANG", "LC_CTYPE", "LC_ALL"] {
            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.env_clear();
            cmd.env("LANG", "C");
            cmd.env(key, &utf8);
            configure_utf8_locale(&mut cmd);
            assert_eq!(cmd.get_env(key), Some(utf8.as_os_str()));
            if key != "LC_CTYPE" {
                assert!(cmd.get_env("LC_CTYPE").is_none());
            }
            if key != "LC_ALL" {
                assert!(cmd.get_env("LC_ALL").is_none());
            }
            if key != "LANG" {
                assert_eq!(cmd.get_env("LANG"), Some(std::ffi::OsStr::new("C")));
            }
        }
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
