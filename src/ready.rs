//! Readiness checks: TCP connect, HTTP(S) GET, or a regex on the app's console output.
//! Each runs on its own thread until it passes or is cancelled.

use crate::config::{ReadyConfig, TcpTarget};
use regex_lite::Regex;
use std::io::{Read, Seek, SeekFrom};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

/// The state of a health watch: the app's `ready` check, rerun every interval.
pub struct Health {
    healthy: AtomicBool,
    /// Failed checks in a row.
    failures: AtomicU32,
    cancel: AtomicBool,
}

impl Health {
    pub fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    pub fn failures(&self) -> u32 {
        self.failures.load(Ordering::Acquire)
    }
}

/// Stops the watch's thread when dropped.
pub struct HealthGuard(pub Arc<Health>);

impl Drop for HealthGuard {
    fn drop(&mut self) {
        self.0.cancel.store(true, Ordering::Release);
    }
}

/// Rerun `check` every `interval`. The app turns unhealthy after `failures` failed
/// checks in a row, and healthy again on the next one that passes.
pub fn watch(mut check: Check, interval: Duration, failures: u32) -> HealthGuard {
    let health = Arc::new(Health {
        healthy: AtomicBool::new(true),
        failures: AtomicU32::new(0),
        cancel: AtomicBool::new(false),
    });
    let h = health.clone();
    std::thread::spawn(move || {
        let mut tail = String::new();
        loop {
            // Sleep in short steps so a cancelled watch stops promptly.
            let wake = std::time::Instant::now() + interval;
            while std::time::Instant::now() < wake {
                if h.cancel.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100).min(interval));
            }
            let ok = match &mut check {
                Check::Tcp(addr) => tcp_ok(addr),
                Check::Http(url) => http_ok(url),
                Check::Log { path, offset, re } => log_matches(path, offset, re, &mut tail),
            };
            if h.cancel.load(Ordering::Acquire) {
                return;
            }
            if ok {
                h.failures.store(0, Ordering::Release);
                h.healthy.store(true, Ordering::Release);
            } else {
                let n = h.failures.fetch_add(1, Ordering::AcqRel) + 1;
                if n >= failures.max(1) {
                    h.healthy.store(false, Ordering::Release);
                }
            }
        }
    });
    HealthGuard(health)
}

fn tcp_ok(addr: &str) -> bool {
    // `localhost` resolves to both ::1 and 127.0.0.1; servers often listen on only one.
    addr.to_socket_addrs().map(|addrs| addrs.into_iter().any(connects)).unwrap_or(false)
}

/// Windows retries a refused connection for about 2s before failing, so a closed port
/// would hold up every check (twice over for `localhost`). Cap the wait instead: on
/// loopback, a listening port accepts at once.
fn connects(a: SocketAddr) -> bool {
    let timeout = Duration::from_millis(if a.ip().is_loopback() { 100 } else { 500 });
    TcpStream::connect_timeout(&a, timeout).is_ok()
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
    // A character cut off at the end of the file is read again, whole, next time.
    let used = match std::str::from_utf8(&bytes) {
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        _ => n,
    };
    *offset += used as u64;
    tail.push_str(&String::from_utf8_lossy(&bytes[..used]));
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
        // WinHTTP waits out Windows' retries on a closed port; check it's open first.
        let host_name = host.to_string_lossy();
        let host_name = host_name.trim_start_matches('[').trim_end_matches(']');
        if !(host_name, parts.nPort).to_socket_addrs().is_ok_and(|mut addrs| addrs.any(connects)) {
            return None;
        }

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
    fn health_turns_bad_after_failures_and_recovers() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        drop(l);
        let g = watch(Check::Tcp(addr.clone()), Duration::from_millis(20), 3);
        let wait_for = |f: &dyn Fn() -> bool| {
            let end = std::time::Instant::now() + Duration::from_secs(5);
            while !f() && std::time::Instant::now() < end {
                std::thread::sleep(Duration::from_millis(10));
            }
            f()
        };
        assert!(wait_for(&|| !g.0.healthy()));
        assert!(g.0.failures() >= 3);
        let _l = TcpListener::bind(&addr).unwrap();
        assert!(wait_for(&|| g.0.healthy() && g.0.failures() == 0));
    }

    #[test]
    fn closed_ports_fail_fast() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let start = std::time::Instant::now();
        assert!(!tcp_ok(&format!("localhost:{port}")));
        assert!(!http_ok(&format!("http://localhost:{port}/")));
        assert!(start.elapsed() < Duration::from_millis(1000), "took {:?}", start.elapsed());
    }

    #[test]
    fn http_check_accepts_any_status_below_500() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut statuses = ["404 Not Found", "503 Service Unavailable"].into_iter().peekable();
            while let Some(status) = statuses.peek() {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = [0u8; 1024];
                // The port check before each request connects and sends nothing.
                if s.read(&mut buf).unwrap_or(0) == 0 {
                    continue;
                }
                let status = *status;
                statuses.next();
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

    #[test]
    fn log_check_matches_non_ascii_split_across_reads() {
        let path = std::env::temp_dir().join(format!("bb-ready-utf8-{}.log", std::process::id()));
        std::fs::write(&path, "").unwrap();
        let mut offset = 0;
        let re = Regex::new("➜  Local:").unwrap();
        let mut tail = String::new();
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        // ➜ is E2 9E 9C; the first read sees only its first two bytes.
        f.write_all(b"  \xE2\x9E").unwrap();
        assert!(!log_matches(&path, &mut offset, &re, &mut tail));
        f.write_all(b"\x9C  Local:    https://localhost:8000/\r\n").unwrap();
        assert!(log_matches(&path, &mut offset, &re, &mut tail));
        let _ = std::fs::remove_file(&path);
    }
}
