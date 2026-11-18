//! Talking to a hub server from outside: is it up, what is it doing, start it, stop it.

use crate::rpc::read_line;
use crate::{logger, paths, proc, rpc, token};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EngineStatus {
    /// `starting`, `ready`, `failed` or `stopped`.
    pub state: String,
    pub pid: u32,
    pub started_at: u64,
    pub restarts: u64,
    /// The engine's own name and version.
    pub server: String,
    /// The command line it was started with (without a library).
    pub command: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Status {
    pub app: String,
    pub port: u16,
    pub pid: u32,
    pub version: String,
    pub started_at: u64,
    pub engine: EngineStatus,
    pub library: Option<String>,
    pub sessions: usize,
    pub session_names: Vec<String>,
    pub requests: u64,
    pub pending: usize,
    pub last_error: String,
    pub stderr_tail: Vec<String>,
}

pub enum Probe {
    /// Nothing is listening on the port.
    Down,
    /// Something that isn't a hub server we can talk to; the text says what.
    Foreign(String),
    Up(Box<Status>),
}

/// Whether something has the port. Asked by trying to take it ourselves, which answers at once:
/// connecting to a closed local port takes Windows about two seconds to give up on.
pub fn port_in_use(port: u16) -> bool {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err()
}

pub struct Conn {
    reader: BufReader<TcpStream>,
    stream: TcpStream,
}

impl Conn {
    /// Connects to the server on the port and presents the hub token.
    pub fn open(port: u16, timeout: Duration) -> Option<Conn> {
        let stream = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), timeout).ok()?;
        let _ = stream.set_nodelay(true);
        let mut conn = Conn { reader: BufReader::new(stream.try_clone().ok()?), stream };
        conn.send_line(&json!({ "hub": "hello", "token": token::get(), "v": 1 }).to_string()).then_some(conn)
    }

    pub fn send_line(&mut self, line: &str) -> bool {
        let mut data = Vec::with_capacity(line.len() + 1);
        data.extend_from_slice(line.as_bytes());
        data.push(b'\n');
        self.stream.write_all(&data).is_ok()
    }

    /// The next line, or `None` when the server closed the connection.
    pub fn read_line(&mut self) -> Option<String> {
        read_line(&mut self.reader)
    }

    /// A second handle on the connection, for writing while another thread reads.
    pub fn writer(&self) -> Option<TcpStream> {
        self.stream.try_clone().ok()
    }

    /// Sends a request and returns its answer, skipping anything else that arrives meanwhile.
    /// `None` when no answer comes in time. A refused token comes back as `{"hub":"denied"}`.
    pub fn call(&mut self, id: &str, method: &str, params: Option<Value>, timeout: Duration) -> Option<Value> {
        let mut request = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        if let Some(params) = params {
            request["params"] = params;
        }
        if !self.send_line(&request.to_string()) {
            return None;
        }
        let deadline = Instant::now() + timeout;
        let _ = self.stream.set_read_timeout(Some(timeout));
        let answer = loop {
            let Some(line) = self.read_line() else { break None };
            let Ok(value) = serde_json::from_str::<Value>(&line) else { break None };
            if value.get("id").and_then(Value::as_str) == Some(id) || value.get("hub").is_some() {
                break Some(value);
            }
            if Instant::now() >= deadline {
                break None;
            }
        };
        let _ = self.stream.set_read_timeout(None);
        answer
    }
}

/// What is on the port.
pub fn status(port: u16) -> Probe {
    if !port_in_use(port) {
        return Probe::Down;
    }
    // Another status check may have been holding the port for an instant: look twice before
    // deciding that something else owns it.
    let mut conn = Conn::open(port, Duration::from_millis(700));
    if conn.is_none() {
        thread::sleep(Duration::from_millis(150));
        if !port_in_use(port) {
            return Probe::Down;
        }
        conn = Conn::open(port, Duration::from_millis(700));
    }
    let Some(mut conn) = conn else { return Probe::Foreign("a program that does not answer".to_string()) };
    match conn.call("hub-status", "hub/status", None, Duration::from_secs(3)) {
        Some(answer) if answer.get("hub") == Some(&json!("denied")) => {
            Probe::Foreign("a hub server started from another hub folder (its token differs)".to_string())
        }
        Some(answer) => match answer.get("result").cloned().and_then(|r| serde_json::from_value::<Status>(r).ok()) {
            Some(status) if !status.app.is_empty() => Probe::Up(Box::new(status)),
            _ => Probe::Foreign("another program".to_string()),
        },
        None => Probe::Foreign("another program".to_string()),
    }
}

/// Starts the app's server unless it is already up, and waits until it answers.
pub fn ensure_server(app: &str, port: u16, wait: Duration) -> Result<Status, String> {
    let check = |probe: Probe| match probe {
        Probe::Up(status) if status.app == app => Ok(Some(*status)),
        Probe::Up(status) => Err(format!("port {port} is already used by the hub server of {}", status.app)),
        Probe::Foreign(who) => Err(format!("port {port} is already used by {who}")),
        Probe::Down => Ok(None),
    };
    if let Some(status) = check(status(port))? {
        return Ok(status);
    }
    let exe = paths::hub_exe();
    proc::spawn_detached(&exe, &["serve".to_string(), app.to_string()]).map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(150));
        if let Some(status) = check(status(port))? {
            return Ok(status);
        }
    }
    Err(format!("the server did not start on port {port} within {} s (log: {})", wait.as_secs(), logger::server_log(app).display()))
}

/// Stops the app's server if it is the one on the port. Returns whether a server was stopped.
pub fn shutdown(port: u16, app: &str) -> bool {
    let Probe::Up(running) = status(port) else { return false };
    if running.app != app {
        return false;
    }
    if let Some(mut conn) = Conn::open(port, Duration::from_millis(700)) {
        conn.call("hub-stop", "hub/shutdown", None, Duration::from_secs(3));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while port_in_use(port) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    if port_in_use(port) {
        proc::kill(running.pid);
    }
    true
}

/// Closes the library the app's engine has open. Fine when no server is running.
pub fn close_library(port: u16) -> Result<(), String> {
    if !port_in_use(port) {
        return Ok(());
    }
    let mut conn = Conn::open(port, Duration::from_secs(1)).ok_or("the server does not answer")?;
    let answer = conn.call("hub-close", "hub/closeLibrary", None, Duration::from_secs(100)).ok_or("no answer from the server")?;
    let result = answer.get("result").ok_or("the server refused the request")?;
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(())
    } else {
        Err(result.get("error").and_then(Value::as_str).unwrap_or("the library could not be closed").to_string())
    }
}

pub struct CheckOk {
    pub tools: usize,
    pub millis: u128,
    pub server: String,
}

/// A full MCP round trip through the server, as a client session would do: initialize, then list
/// the tools.
pub fn check(port: u16) -> Result<CheckOk, String> {
    let started = Instant::now();
    if !port_in_use(port) {
        return Err(format!("no server is running on port {port}"));
    }
    let mut conn = Conn::open(port, Duration::from_secs(1)).ok_or_else(|| format!("the program on port {port} does not answer"))?;
    let failure = |answer: &Value| answer.get("error").map(|e| e.get("message").and_then(Value::as_str).unwrap_or("error").to_string());
    let params = json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "crafthub-check", "version": crate::VERSION } });
    let init = conn.call("check-1", "initialize", Some(params), Duration::from_secs(30)).ok_or("no answer to initialize")?;
    if init.get("hub").is_some() {
        return Err("the server refused the hub token".to_string());
    }
    if let Some(why) = failure(&init) {
        return Err(why);
    }
    conn.send_line(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string());
    let tools = conn.call("check-2", "tools/list", Some(json!({})), Duration::from_secs(30)).ok_or("no answer to tools/list")?;
    if let Some(why) = failure(&tools) {
        return Err(why);
    }
    Ok(CheckOk {
        tools: tools.get("result").and_then(|r| r.get("tools")).and_then(Value::as_array).map_or(0, Vec::len),
        millis: started.elapsed().as_millis(),
        server: init.get("result").and_then(Value::as_object).map(rpc::server_label).unwrap_or_default(),
    })
}

/// A full MCP round trip directly against the app's command-line tool over stdio (direct mode).
pub fn check_direct(command_line: &[String]) -> Result<CheckOk, String> {
    use std::path::Path;
    use std::process::{Command, Stdio};

    let started = Instant::now();
    let Some((program, args)) = command_line.split_first() else {
        return Err("no engine command".to_string());
    };
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(dir) = Path::new(program).parent().filter(|d| d.is_dir()) {
        command.current_dir(dir);
    }
    let mut child = proc::quiet(&mut command).spawn().map_err(|e| format!("could not start {program}: {e}"))?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        return Err("could not open engine stdio pipes".to_string());
    };
    let init_req = json!({
        "jsonrpc": "2.0",
        "id": "check-1",
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "crafthub-check", "version": crate::VERSION }
        }
    });
    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let tools_req = json!({ "jsonrpc": "2.0", "id": "check-2", "method": "tools/list", "params": {} });
    let payload = format!("{init_req}\n{initialized}\n{tools_req}\n");
    if stdin.write_all(payload.as_bytes()).is_err() || stdin.flush().is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("could not write MCP initialize to engine stdin".to_string());
    }
    let mut reader = BufReader::new(stdout);
    let mut server = String::new();
    let mut tools_count = None;
    let mut err_msg = None;
    while let Some(line) = read_line(&mut reader) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        match value.get("id").and_then(Value::as_str) {
            Some("check-1") => {
                if let Some(err) = value.get("error") {
                    err_msg = Some(err.get("message").and_then(Value::as_str).unwrap_or("initialize error").to_string());
                    break;
                }
                server = value.get("result").and_then(Value::as_object).map(rpc::server_label).unwrap_or_default();
            }
            Some("check-2") => {
                if let Some(err) = value.get("error") {
                    err_msg = Some(err.get("message").and_then(Value::as_str).unwrap_or("tools/list error").to_string());
                    break;
                }
                tools_count = Some(value.get("result").and_then(|r| r.get("tools")).and_then(Value::as_array).map_or(0, Vec::len));
                break;
            }
            _ => {}
        }
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    if let Some(why) = err_msg {
        return Err(why);
    }
    let tools = tools_count.ok_or_else(|| "engine exited before answering tools/list".to_string())?;
    Ok(CheckOk { tools, millis: started.elapsed().as_millis(), server })
}
