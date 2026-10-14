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
}

impl SyncServer {
    /// Start a server on a free loopback port.
    ///
    /// # Panics
    ///
    /// Panics when `TURSODB_BIN` is not set or the server does not start.
    pub fn start() -> Self {
        let bin = std::env::var("TURSODB_BIN").expect("set TURSODB_BIN to the tursodb binary");
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("free port")
            .port();
        let addr = format!("127.0.0.1:{port}");
        let child = Command::new(bin)
            .args(["--sync-server", &addr])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn tursodb");
        let started = Instant::now();
        while TcpStream::connect(&addr).is_err() {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "sync server did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Self {
            child,
            url: format!("http://{addr}"),
        }
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
