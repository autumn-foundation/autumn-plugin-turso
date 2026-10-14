//! A local `tursodb --sync-server` for sync tests.
//!
//! Set `TURSODB_BIN` to the `tursodb` binary path.

use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A running sync server. Drop stops it.
pub struct SyncServer {
    child: Child,
    url: String,
    _log_dir: tempfile::TempDir,
}

impl SyncServer {
    /// Start a server on a free loopback port. Try a new port when the
    /// server stops at start.
    ///
    /// # Panics
    ///
    /// Panics when `TURSODB_BIN` is not set or no server starts.
    pub fn start() -> Self {
        let bin = std::env::var("TURSODB_BIN").expect("set TURSODB_BIN to the tursodb binary");
        let mut last_log = String::new();
        for _ in 0..5 {
            let port = TcpListener::bind("127.0.0.1:0")
                .and_then(|listener| listener.local_addr())
                .expect("free port")
                .port();
            let addr = format!("127.0.0.1:{port}");
            let log_dir = tempfile::tempdir().expect("log dir");
            let log_path = log_dir.path().join("tursodb.log");
            let log = std::fs::File::create(&log_path).expect("log file");
            let mut child = Command::new(&bin)
                .args(["--sync-server", &addr])
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .expect("spawn tursodb");
            let started = Instant::now();
            loop {
                if let Some(status) = child.try_wait().expect("poll tursodb") {
                    last_log = format!(
                        "exit {status}: {}",
                        std::fs::read_to_string(&log_path).unwrap_or_default()
                    );
                    break;
                }
                if TcpStream::connect(&addr).is_ok() {
                    return Self {
                        child,
                        url: format!("http://{addr}"),
                        _log_dir: log_dir,
                    };
                }
                if started.elapsed() > Duration::from_secs(30) {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("sync server on {addr} did not start in 30s");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        panic!("sync server did not start: {last_log}");
    }

    /// The server URL.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for SyncServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
