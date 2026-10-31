//! Pseudo-terminal: open a pty pair and run the shell (or -e command) on it.
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

pub struct Pty {
    pub master: OwnedFd,
    pub pid: libc::pid_t,
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap()
}

impl Pty {
    /// Spawn `argv` (or the user's shell) on a new pty of `cols`x`rows`.
    pub fn spawn(argv: &[String], cols: u16, rows: u16, cwd: Option<&str>, extra_env: &[(String, String)]) -> io::Result<Pty> {
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
            if master < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut name = [0 as libc::c_char; 128];
            if libc::ptsname_r(master, name.as_mut_ptr(), name.len()) != 0 {
                return Err(io::Error::last_os_error());
            }
            let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
            libc::ioctl(master, libc::TIOCSWINSZ, &ws);

            let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
            let argv: Vec<String> = if argv.is_empty() { vec![shell.clone()] } else { argv.to_vec() };
            let c_argv: Vec<CString> = argv.iter().map(|a| cstr(a)).collect();
            let mut p_argv: Vec<*const libc::c_char> = c_argv.iter().map(|c| c.as_ptr()).collect();
            p_argv.push(std::ptr::null());
            // login-shell style argv[0] ("-bash") only when running the default shell
            let login0 = if argv.len() == 1 && argv[0] == shell {
                Some(cstr(&format!("-{}", shell.rsplit('/').next().unwrap_or("sh"))))
            } else {
                None
            };
            let mut envs: Vec<CString> = std::env::vars()
                .filter(|(k, _)| !matches!(k.as_str(), "TERM" | "COLORTERM" | "SHADOW_TERM" | "TERM_PROGRAM" | "LINES" | "COLUMNS"))
                .map(|(k, v)| cstr(&format!("{k}={v}")))
                .collect();
            envs.push(cstr("TERM=xterm-256color"));
            envs.push(cstr("COLORTERM=truecolor"));
            envs.push(cstr("TERM_PROGRAM=shadow-term"));
            envs.push(cstr("SHADOW_TERM=1"));
            for (k, v) in extra_env {
                envs.push(cstr(&format!("{k}={v}")));
            }
            let mut p_env: Vec<*const libc::c_char> = envs.iter().map(|c| c.as_ptr()).collect();
            p_env.push(std::ptr::null());
            let slave_name = CString::from_raw(libc::strdup(name.as_ptr()));
            let cwd_c = cwd.map(cstr);

            let pid = libc::fork();
            if pid < 0 {
                return Err(io::Error::last_os_error());
            }
            if pid == 0 {
                // child: new session, slave becomes controlling tty
                libc::setsid();
                let slave = libc::open(slave_name.as_ptr(), libc::O_RDWR);
                if slave < 0 {
                    libc::_exit(126);
                }
                libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
                libc::dup2(slave, 0);
                libc::dup2(slave, 1);
                libc::dup2(slave, 2);
                if slave > 2 {
                    libc::close(slave);
                }
                libc::close(master);
                if let Some(c) = &cwd_c {
                    libc::chdir(c.as_ptr());
                }
                for sig in [libc::SIGCHLD, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGALRM, libc::SIGPIPE] {
                    libc::signal(sig, libc::SIG_DFL);
                }
                let prog = c_argv[0].as_ptr();
                if let Some(l0) = &login0 {
                    p_argv[0] = l0.as_ptr();
                }
                libc::execvpe(prog, p_argv.as_ptr(), p_env.as_ptr());
                // exec failed: report on the pty and exit
                let msg = format!("shadow-term: cannot run {:?}\r\n", argv[0]);
                libc::write(1, msg.as_ptr() as *const _, msg.len());
                libc::_exit(127);
            }
            // parent
            let flags = libc::fcntl(master, libc::F_GETFL);
            libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
            Ok(Pty { master: OwnedFd::from_raw_fd(master), pid })
        }
    }

    pub fn fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    pub fn resize(&self, cols: u16, rows: u16, px_w: u16, px_h: u16) {
        let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: px_w, ws_ypixel: px_h };
        unsafe {
            libc::ioctl(self.fd(), libc::TIOCSWINSZ, &ws);
        }
    }

    /// Non-blocking read. Ok(0) = child closed the pty.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe { libc::read(self.fd(), buf.as_mut_ptr() as *mut _, buf.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            // EIO = slave side closed (child exited)
            if e.raw_os_error() == Some(libc::EIO) {
                return Ok(0);
            }
            return Err(e);
        }
        Ok(n as usize)
    }

    /// Write everything (retries on EAGAIN briefly so big pastes go through).
    pub fn write_all(&self, mut data: &[u8]) {
        let mut spins = 0;
        while !data.is_empty() {
            let n = unsafe { libc::write(self.fd(), data.as_ptr() as *const _, data.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::WouldBlock && spins < 2000 {
                    spins += 1;
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                return;
            }
            data = &data[n as usize..];
        }
    }

    /// Exit status if the child has exited.
    pub fn try_wait(&self) -> Option<i32> {
        let mut status = 0;
        let r = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        if r == self.pid {
            if libc::WIFEXITED(status) {
                Some(libc::WEXITSTATUS(status))
            } else {
                Some(128 + libc::WTERMSIG(status))
            }
        } else {
            None
        }
    }

    /// Current working directory of the child (fallback when the shell doesn't send OSC 7).
    pub fn cwd(&self) -> Option<String> {
        // the foreground process group leader is the most relevant process
        let fg = unsafe { libc::tcgetpgrp(self.fd()) };
        let pid = if fg > 0 { fg } else { self.pid };
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok().map(|p| p.to_string_lossy().to_string())
    }

    /// Name of the foreground program (for the HUD), e.g. "vim".
    pub fn foreground(&self) -> Option<String> {
        let fg = unsafe { libc::tcgetpgrp(self.fd()) };
        if fg <= 0 {
            return None;
        }
        std::fs::read_to_string(format!("/proc/{fg}/comm")).ok().map(|s| s.trim().to_string())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.pid, libc::SIGHUP);
        }
    }
}
