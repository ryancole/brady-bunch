//! Readiness checks: TCP connect, HTTP(S) GET, or a regex on the app's console output.
//! Each runs on its own thread until it passes or is cancelled.

use crate::config::{ReadyConfig, TcpTarget};
use regex_lite::Regex;
use std::io::{Read, Seek, SeekFrom};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, PCWSTR, w};

const INTERVAL: Duration = Duration::from_millis(250);

pub enum Check {
    Tcp(String),
    Http(String),
    Log { path: PathBuf, offset: u64, re: Regex },
}

impl Check {
    /// The check for `ready`. `log` is where the app's output is copied, if anywhere.
    pub fn from_config(r: &ReadyConfig, log: Option<(PathBuf, u64)>) -> Option<Check> {
        if let Some(t) = &r.tcp {
            return Some(Check::Tcp(match t {
                TcpTarget::Port(p) => format!("localhost:{p}"),
                TcpTarget::Addr(a) => a.clone(),
            }));
        }
        if let Some(u) = &r.http {
            return Some(Check::Http(u.clone()));
        }
        let (path, offset) = log?;
        Some(Check::Log { path, offset, re: Regex::new(r.log.as_deref()?).ok()? })
    }

    /// Short description for status and the event log, e.g. `tcp localhost:10000`.
    pub fn describe(r: &ReadyConfig) -> String {
        match (&r.tcp, &r.http, &r.log) {
            (Some(TcpTarget::Port(p)), _, _) => format!("tcp localhost:{p}"),
            (Some(TcpTarget::Addr(a)), _, _) => format!("tcp {a}"),
            (_, Some(u), _) => format!("http {u}"),
            (_, _, Some(re)) => format!("log /{re}/"),
            _ => String::new(),
        }
    }
}

pub struct Probe {
    passed: AtomicBool,
    finished: AtomicBool,
    cancel: AtomicBool,
}

impl Probe {
    pub fn passed(&self) -> bool {
        self.passed.load(Ordering::Acquire)
    }

    /// True once a one-shot probe has made its single attempt.
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}

/// Stops the probe's thread when dropped.
pub struct ProbeGuard(pub Arc<Probe>);

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        self.0.cancel.store(true, Ordering::Release);
    }
}

/// Run `check` until it passes, or just once if `once`.
pub fn spawn(mut check: Check, once: bool) -> ProbeGuard {
    let probe = Arc::new(Probe {
        passed: AtomicBool::new(false),
        finished: AtomicBool::new(false),
        cancel: AtomicBool::new(false),
    });
    let p = probe.clone();
    std::thread::spawn(move || {
        let mut tail = String::new();
        loop {
            if p.cancel.load(Ordering::Acquire) {
                return;
            }
            let ok = match &mut check {
                Check::Tcp(addr) => tcp_ok(addr),
                Check::Http(url) => http_ok(url),
                Check::Log { path, offset, re } => log_matches(path, offset, re, &mut tail),
            };
            if ok {
                p.passed.store(true, Ordering::Release);
            }
            if ok || once {
                p.finished.store(true, Ordering::Release);
                return;
            }
            std::thread::sleep(INTERVAL);
        }
    });
    ProbeGuard(probe)
}

fn tcp_ok(addr: &str) -> bool {
    // `localhost` resolves to both ::1 and 127.0.0.1; servers often listen on only one.
    addr.to_socket_addrs()
        .map(|addrs| addrs.into_iter().any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(500)).is_ok()))
        .unwrap_or(false)
}

/// New output in the log file (from `offset` on) matches `re`. `tail` carries the end
/// of the previous read so a match split across reads isn't missed.
fn log_matches(path: &PathBuf, offset: &mut u64, re: &Regex, tail: &mut String) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    if f.seek(SeekFrom::Start(*offset)).is_err() {
        return false;
    }
    let mut bytes = Vec::new();
    let Ok(n) = f.read_to_end(&mut bytes) else { return false };
    if n == 0 {
        return false;
    }
    *offset += n as u64;
    tail.push_str(&String::from_utf8_lossy(&bytes));
    if re.is_match(tail) {
        return true;
    }
    const KEEP: usize = 4096;
    if tail.len() > KEEP {
        let mut cut = tail.len() - KEEP;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail.drain(..cut);
    }
    false
}

/// Any HTTP response below 500. Certificate errors are ignored: local dev servers
/// usually have self-signed certificates.
pub fn http_ok(url: &str) -> bool {
    http_status(url).is_some_and(|s| s < 500)
}

struct Internet(*mut core::ffi::c_void);

impl Drop for Internet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn http_status(url: &str) -> Option<u32> {
    unsafe {
        let wide: Vec<u16> = url.encode_utf16().collect();
        let mut host = vec![0u16; 256];
        let mut path = vec![0u16; 2048];
        let mut parts = URL_COMPONENTS {
            dwStructSize: size_of::<URL_COMPONENTS>() as u32,
            lpszHostName: windows::core::PWSTR(host.as_mut_ptr()),
            dwHostNameLength: host.len() as u32,
            lpszUrlPath: windows::core::PWSTR(path.as_mut_ptr()),
            dwUrlPathLength: path.len() as u32,
            ..Default::default()
        };
        WinHttpCrackUrl(&wide, 0, &mut parts).ok()?;
        let host = HSTRING::from_wide(&host[..parts.dwHostNameLength as usize]);
        let path = HSTRING::from_wide(&path[..parts.dwUrlPathLength as usize]);
        let secure = parts.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS;

        let session = Internet(WinHttpOpen(
            w!("brady-bunch"),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return None;
        }
        WinHttpSetTimeouts(session.0, 2000, 2000, 2000, 2000).ok()?;
        let connect = Internet(WinHttpConnect(session.0, &host, parts.nPort, 0));
        if connect.0.is_null() {
            return None;
        }
        let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let request = Internet(WinHttpOpenRequest(
            connect.0,
            w!("GET"),
            &path,
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        ));
        if request.0.is_null() {
            return None;
        }
        if secure {
            let ignore = SECURITY_FLAG_IGNORE_UNKNOWN_CA
                | SECURITY_FLAG_IGNORE_CERT_DATE_INVALID
                | SECURITY_FLAG_IGNORE_CERT_CN_INVALID
                | SECURITY_FLAG_IGNORE_CERT_WRONG_USAGE;
            let _ = WinHttpSetOption(Some(request.0), WINHTTP_OPTION_SECURITY_FLAGS, Some(&ignore.to_ne_bytes()));
        }
        // A redirect is an answer; following it could lead somewhere that isn't up.
        let no_redirects = WINHTTP_DISABLE_REDIRECTS;
        let _ = WinHttpSetOption(Some(request.0), WINHTTP_OPTION_DISABLE_FEATURE, Some(&no_redirects.to_ne_bytes()));
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).ok()?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).ok()?;
        let mut status = 0u32;
        let mut len = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut core::ffi::c_void),
            &mut len,
            std::ptr::null_mut(),
        )
        .ok()?;
        Some(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    #[test]
    fn tcp_check_sees_a_listener() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(tcp_ok(&format!("localhost:{port}")));
        drop(l);
        assert!(!tcp_ok(&format!("127.0.0.1:{port}")));
    }

    #[test]
    fn http_check_accepts_any_status_below_500() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for status in ["404 Not Found", "503 Service Unavailable"] {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        let url = format!("http://127.0.0.1:{port}/health");
        assert!(http_ok(&url));
        assert!(!http_ok(&url));
        server.join().unwrap();
    }

    #[test]
    fn log_check_matches_across_reads_and_skips_old_output() {
        let path = std::env::temp_dir().join(format!("bb-ready-{}.log", std::process::id()));
        std::fs::write(&path, "Now listening on: old\n").unwrap();
        let mut offset = std::fs::metadata(&path).unwrap().len();
        let re = Regex::new("Now listening on: https").unwrap();
        let mut tail = String::new();
        assert!(!log_matches(&path, &mut offset, &re, &mut tail));
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(f, "Now listen").unwrap();
        assert!(!log_matches(&path, &mut offset, &re, &mut tail));
        writeln!(f, "ing on: https://localhost:8002").unwrap();
        assert!(log_matches(&path, &mut offset, &re, &mut tail));
        let _ = std::fs::remove_file(&path);
    }
}
