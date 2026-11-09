//! A libSQL database for one test.
//!
//! A database has one writer at a time, so tests sharing one wait on each other
//! and one stuck writer stalls them all. Each test therefore starts its own
//! server, from the executable `CUENV_INFRASTRUCTURE_TEST_SQLD` names, unless
//! `CUENV_INFRASTRUCTURE_TEST_TURSO_URL` names a database to use instead.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, thread};

const EXECUTABLE: &str = "CUENV_INFRASTRUCTURE_TEST_SQLD";
const URL: &str = "CUENV_INFRASTRUCTURE_TEST_TURSO_URL";

/// Another process can take a free port before the server binds it.
const START_ATTEMPTS: u32 = 5;
const READY_WITHIN: Duration = Duration::from_secs(30);

/// A database, kept available while this value lives.
pub struct Database {
    url: String,
    _server: Option<Server>,
}

impl Database {
    /// The database a test should use, or `None` if nothing is configured.
    pub fn configured() -> io::Result<Option<Self>> {
        if let Ok(url) = env::var(URL) {
            return Ok(Some(Self { url, _server: None }));
        }
        if env::var_os(EXECUTABLE).is_none() {
            return Ok(None);
        }
        Self::isolated().map(Some)
    }

    /// The database a test should use, which has to be configured.
    pub fn start() -> io::Result<Self> {
        Self::configured()?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{EXECUTABLE} must point at sqld, or {URL} at a database"),
            )
        })
    }

    /// A new, empty database nothing else uses.
    pub fn isolated() -> io::Result<Self> {
        let executable = env::var_os(EXECUTABLE).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{EXECUTABLE} must point at sqld"),
            )
        })?;
        let mut last_error = None;
        for _ in 0..START_ATTEMPTS {
            match Server::start(Path::new(&executable)) {
                Ok(server) => {
                    return Ok(Self {
                        url: format!("http://127.0.0.1:{}", server.port),
                        _server: Some(server),
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| io::Error::other("sqld was not started")))
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

struct Server {
    port: u16,
    process: Child,
    _directory: tempfile::TempDir,
}

impl Server {
    fn start(executable: &Path) -> io::Result<Self> {
        let directory = tempfile::tempdir()?;
        let log = directory.path().join("sqld.log");
        let data = directory.path().join("data");
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port();
        let output = File::create(&log)?;
        let process = Command::new(executable)
            .arg("--http-listen-addr")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--db-path")
            .arg(&data)
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output)
            .spawn()?;
        let mut server = Self {
            port,
            process,
            _directory: directory,
        };
        let deadline = Instant::now() + READY_WITHIN;
        while Instant::now() < deadline {
            if let Some(status) = server.process.try_wait()? {
                let log = fs::read_to_string(&log).unwrap_or_default();
                return Err(io::Error::other(format!("sqld exited ({status}): {log}")));
            }
            // A server creates its database only after binding its port, so a
            // healthy answer with this directory present comes from this one
            // and not from another that took the port first.
            if data.join("dbs/default").exists() && answers_health_check(port) {
                return Ok(server);
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(io::Error::other(format!(
            "sqld did not become healthy within {READY_WITHIN:?}"
        )))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Nothing can be done about a server that will not stop.
        drop(self.process.kill());
        drop(self.process.wait());
    }
}

fn answers_health_check(port: u16) -> bool {
    let check = || -> io::Result<bool> {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        stream.write_all(b"GET /health HTTP/1.0\r\n\r\n")?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response.contains(" 200 "))
    };
    check().unwrap_or(false)
}
