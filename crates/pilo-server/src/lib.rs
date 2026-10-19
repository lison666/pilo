mod chat_files;
mod command;
mod environment;
mod extensions;
pub use extensions::runtime_directory;
mod fs_ops;
mod output;
mod runtime_streams;
mod session;

use std::{
    collections::HashMap,
    io::{self, Read},
    path::Path,
    sync::{Arc, Mutex as StdMutex},
    time::Instant,
};

use command::command_run;
use environment::{ToolchainInfo, environment_inspect, environment_pi_probe};
use fs_ops::{
    fs_mkdir, fs_mkdir_absolute, fs_read_dir, fs_read_file, fs_remove, fs_rename, fs_search,
    fs_stat, fs_write_file,
};
use output::ServerWriter;
use pilo_protocol::{
    Envelope, MAX_BINARY_PAYLOAD_BYTES, PROTOCOL_VERSION, SERVER_CAPABILITIES, ServerPing,
    ServerStatus, read_frame, write_frame,
};
use runtime_streams::{
    PiProcess, TerminalSession, pi_send, pi_start, pi_stop, preview_ports, terminal_close,
    terminal_open, terminal_resize, terminal_write,
};
use serde::Deserialize;
use serde_json::Value;
use session::{
    session_activity, session_delete, session_discover, session_read, session_scan, session_search,
    session_watch_start, session_watch_stop,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex, OnceCell, Semaphore, mpsc},
    task::{JoinHandle, JoinSet},
};

pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_IN_FLIGHT_REQUESTS: usize = 64;

pub fn binary_fingerprint(path: &Path) -> io::Result<String> {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut file = std::fs::File::open(path)?;
    let mut hash = FNV_OFFSET;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        for byte in &buffer[..read] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    Ok(format!("{hash:016x}"))
}

type PiStarts = HashMap<u64, (String, tokio::sync::oneshot::Sender<()>)>;
type PiOutputTasks = HashMap<
    String,
    (
        std::sync::Weak<Mutex<tokio::process::ChildStdin>>,
        [JoinHandle<()>; 2],
    ),
>;

#[derive(Clone)]
struct ServerState {
    writer: ServerWriter,
    request_slots: Arc<Semaphore>,
    control_slots: Arc<Semaphore>,
    search_slots: Arc<Semaphore>,
    started_at: Instant,
    pi: Arc<Mutex<HashMap<String, PiProcess>>>,
    pi_starts: Arc<Mutex<PiStarts>>,
    pi_stops: Arc<Mutex<JoinSet<()>>>,
    pi_output_tasks: Arc<StdMutex<PiOutputTasks>>,
    session_watchers: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    terminals: Arc<StdMutex<HashMap<String, TerminalSession>>>,
    login_path: Arc<OnceCell<Mutex<String>>>,
    toolchain: Arc<OnceCell<ToolchainInfo>>,
    extensions_dir: Arc<std::path::PathBuf>,
}

fn new_state(
    writer_tx: mpsc::Sender<output::QueuedOutput>,
    extensions_dir: std::path::PathBuf,
) -> ServerState {
    ServerState {
        writer: ServerWriter::new(writer_tx),
        request_slots: Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS)),
        control_slots: Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS)),
        search_slots: Arc::new(Semaphore::new(2)),
        started_at: Instant::now(),
        pi: Arc::new(Mutex::new(HashMap::new())),
        pi_starts: Arc::new(Mutex::new(HashMap::new())),
        pi_stops: Arc::new(Mutex::new(JoinSet::new())),
        pi_output_tasks: Arc::new(StdMutex::new(HashMap::new())),
        session_watchers: Arc::new(Mutex::new(HashMap::new())),
        terminals: Arc::new(StdMutex::new(HashMap::new())),
        login_path: Arc::new(OnceCell::new()),
        toolchain: Arc::new(OnceCell::new()),
        extensions_dir: Arc::new(extensions_dir),
    }
}

/// Stop and drain every child the server owns: pi processes, stream readers,
/// session watchers, terminals. Mirrors the tail of `serve_stdio`.
async fn drain_children(state: &ServerState) {
    state.pi_starts.lock().await.clear();
    for (_, (_, tasks)) in state
        .pi_output_tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .drain()
    {
        for task in tasks {
            task.abort();
        }
    }
    let processes = state
        .pi
        .lock()
        .await
        .drain()
        .map(|(_, process)| process)
        .collect::<Vec<_>>();
    let mut stops = state.pi_stops.lock().await;
    for process in processes {
        stops.spawn(async move {
            if let Err(error) = process.stop().await {
                eprintln!("[runtime-diagnostics] Pi cleanup failed: {error}");
            }
        });
    }
    while let Some(result) = stops.join_next().await {
        if let Err(error) = result {
            eprintln!("[runtime-diagnostics] Pi cleanup task failed: {error}");
        }
    }
    drop(stops);
    for (_, task) in state.session_watchers.lock().await.drain() {
        task.abort();
    }
    if let Ok(mut terminals) = state.terminals.lock() {
        for (_, mut terminal) in terminals.drain() {
            let _ = terminal.child.kill();
            let _ = terminal.child.wait();
        }
    }
}

/// Print pushed events (pi stdout, session watch, ...) to stderr as JSON
/// lines so `call` mode keeps stdout machine-clean.
fn drain_events(mut rx: mpsc::Receiver<output::QueuedOutput>) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(queued) = rx.recv().await {
            if let Envelope::Event {
                stream_id,
                event,
                data,
                binary,
            } = queued.message
            {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "event": event,
                        "streamId": stream_id,
                        "data": data,
                        "binaryBytes": binary.iter().map(Vec::len).collect::<Vec<_>>(),
                    })
                );
            }
        }
    })
}

/// One-shot request for agent/test use: `pilo-server call <method> [params]`.
/// Returns `(result, binary attachments)` exactly as `dispatch` produced them.
pub async fn call_once(
    method: &str,
    params: Value,
    binary: Vec<Vec<u8>>,
) -> Result<(Value, Vec<Vec<u8>>), String> {
    let extensions_dir = extensions::deploy().map_err(|error| error.to_string())?;
    let (tx, rx) = mpsc::channel::<output::QueuedOutput>(256);
    let events = drain_events(rx);
    let state = new_state(tx, extensions_dir);
    // Spawn like serve_stdio does: dispatch's future is large enough to
    // overflow the caller's stack when awaited inline.
    let op_state = state.clone();
    let method = method.to_owned();
    let reply = tokio::spawn(async move { dispatch(&op_state, &method, params, binary).await })
        .await
        .map_err(|error| format!("call task failed: {error}"))?;
    drain_children(&state).await;
    drop(state);
    let _ = events.await;
    reply.map(|reply| (reply.value, reply.binary))
}

/// JSON-lines request loop for agent/test use: `pilo-server call` with no
/// method reads `{"method":..., "params":..., "binaryHex":"..."}` lines from
/// stdin and writes `{"ok":..., "result"/"error":...}` lines to stdout. All
/// lines share one ServerState, so pi.start/pi.send/pi.stop sequences work.
pub async fn call_jsonl() -> Result<(), Box<dyn std::error::Error>> {
    let extensions_dir = extensions::deploy()?;
    let (tx, rx) = mpsc::channel::<output::QueuedOutput>(256);
    let events = drain_events(rx);
    let state = new_state(tx, extensions_dir);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        // Spawn like serve_stdio does: dispatch's future is large enough to
        // overflow the caller's stack when awaited inline.
        let op_state = state.clone();
        let line = line.trim().to_owned();
        let response =
            match tokio::spawn(async move { handle_call_line(&op_state, &line).await }).await {
                Ok(response) => response,
                Err(error) => {
                    serde_json::json!({"ok": false, "error": format!("call task failed: {error}")})
                }
            };
        out.write_all(response.to_string().as_bytes()).await?;
        out.write_all(b"\n").await?;
        out.flush().await?;
    }
    drain_children(&state).await;
    drop(state);
    let _ = events.await;
    Ok(())
}

async fn handle_call_line(state: &ServerState, line: &str) -> Value {
    let request = match serde_json::from_str::<Value>(line) {
        Ok(request) => request,
        Err(error) => {
            return serde_json::json!({"ok": false, "error": format!("invalid JSON: {error}")});
        }
    };
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return serde_json::json!({"ok": false, "error": "missing 'method'"});
    };
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let binary = match request.get("binaryHex").and_then(Value::as_str) {
        Some(hex) => match hex_decode(hex) {
            Ok(data) => vec![data],
            Err(error) => return serde_json::json!({"ok": false, "error": error}),
        },
        None => Vec::new(),
    };
    match dispatch(state, method, params, binary).await {
        Ok(reply) => serde_json::json!({
            "ok": true,
            "result": reply.value,
            "binaryHex": reply.binary.iter().map(|b| hex_encode(b)).collect::<Vec<_>>(),
        }),
        Err(error) => serde_json::json!({"ok": false, "error": error}),
    }
}

pub fn hex_encode(data: &[u8]) -> String {
    use std::fmt::Write as _;
    data.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

pub fn hex_decode(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return Err("binaryHex must have even length".to_owned());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&text[i..i + 2], 16)
                .map_err(|error| format!("binaryHex invalid at byte {}: {error}", i / 2))
        })
        .collect()
}

pub async fn serve_stdio() -> Result<(), Box<dyn std::error::Error>> {
    let extensions_dir = extensions::deploy()?;
    eprintln!(
        "[runtime-diagnostics] server started pid={} version={SERVER_VERSION} maxInFlight={MAX_IN_FLIGHT_REQUESTS}",
        std::process::id()
    );
    let (writer_tx, mut writer_rx) = mpsc::channel::<output::QueuedOutput>(256);
    let state = new_state(writer_tx, extensions_dir);

    let (writer_done_tx, mut writer_done_rx) = tokio::sync::oneshot::channel();
    let mut writer_task = tokio::spawn(async move {
        let stdout = tokio::io::stdout();
        let mut stdout = stdout;
        while let Some(queued) = writer_rx.recv().await {
            // Keep the byte reservation until the frame has actually been written.
            if let Err(error) = write_frame(&mut stdout, queued.message).await {
                eprintln!(
                    "[runtime-diagnostics] server stdout failed pid={} kind={:?} error={error}",
                    std::process::id(),
                    error.kind()
                );
                break;
            }
        }
        let _ = writer_done_tx.send(());
    });

    let stdin = tokio::io::stdin();
    let mut stdin = stdin;
    let mut requests = JoinSet::new();
    let read_result = loop {
        while let Some(result) = requests.try_join_next() {
            if let Err(error) = result {
                eprintln!(
                    "[runtime-diagnostics] server request task failed pid={} panic={} cancelled={}",
                    std::process::id(),
                    error.is_panic(),
                    error.is_cancelled()
                );
            }
        }
        let frame = tokio::select! {
            frame = read_frame(&mut stdin) => frame,
            _ = &mut writer_done_rx => break Ok(()),
        };
        let message = match frame {
            Ok(Some(message)) => message,
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        };
        let Envelope::Request {
            id,
            method,
            params,
            binary,
        } = message
        else {
            continue;
        };
        let slots = if matches!(
            method.as_str(),
            "pi.stop" | "terminal.close" | "session.watch_stop"
        ) {
            &state.control_slots
        } else {
            &state.request_slots
        };
        // Never stop reading stdin while waiting for admission: EOF and stop
        // requests must still reach cleanup when ordinary requests are saturated.
        let permit = match Arc::clone(slots).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let response =
                    Envelope::error(id, "server_busy", "pilo-server request capacity exhausted");
                match tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    state.writer.send(response),
                )
                .await
                {
                    Ok(Ok(())) => continue,
                    _ => break Ok(()),
                }
            }
        };
        // Register starts in input order, before their tasks can await probing.
        let start_cancel = if method == "pi.start"
            && let Some(stream_id) = params.get("streamId").and_then(Value::as_str)
        {
            let (cancel, cancelled) = tokio::sync::oneshot::channel();
            state
                .pi_starts
                .lock()
                .await
                .insert(id, (stream_id.to_owned(), cancel));
            Some(cancelled)
        } else {
            None
        };
        let state = state.clone();
        requests.spawn(async move {
            let _permit = permit;
            let started = Instant::now();
            let stream_id = params.get("streamId").and_then(Value::as_str).map(str::to_owned);
            let rpc_id = params.pointer("/command/id").and_then(Value::as_str).map(str::to_owned);
            let operation = dispatch(&state, &method, params, binary);
            let result = if let Some(cancelled) = start_cancel {
                tokio::select! {
                    biased;
                    _ = cancelled => Err("Pi stream startup cancelled".to_owned()),
                    result = operation => result,
                }
            } else {
                operation.await
            };
            if method == "pi.start" {
                state.pi_starts.lock().await.remove(&id);
            }
            if result.is_err() || started.elapsed().as_secs() >= 5 {
                eprintln!("[runtime-diagnostics] server request pid={} requestId={id} method={method} streamId={stream_id:?} rpcId={rpc_id:?} ok={} elapsedMs={} activeRequests={}", std::process::id(), result.is_ok(), started.elapsed().as_millis(), MAX_IN_FLIGHT_REQUESTS.saturating_sub(state.request_slots.available_permits()));
            }
            let response = match result {
                Ok(reply) => Envelope::response_with_binary(id, reply.value, reply.binary),
                Err(error) => Envelope::error(id, "request_failed", error),
            };
            if state.writer.send(response).await.is_err() {
                eprintln!("[runtime-diagnostics] server response queue closed pid={} requestId={id} method={method}", std::process::id());
            }
        });
    };

    eprintln!(
        "[runtime-diagnostics] server stdin ended pid={} reason={:?} activeRequests={}",
        std::process::id(),
        read_result.as_ref().err().map(std::io::Error::kind),
        requests.len()
    );
    // Cancel pending pi.start before draining the registry, so no request can
    // spawn a new child after shutdown cleanup has already passed it.
    requests.abort_all();
    while requests.join_next().await.is_some() {}
    let pi_process_count = state.pi.lock().await.len();
    eprintln!(
        "[runtime-diagnostics] server cleanup pid={} piProcesses={}",
        std::process::id(),
        pi_process_count
    );
    drain_children(&state).await;
    drop(state.writer);
    if tokio::time::timeout(std::time::Duration::from_secs(1), &mut writer_task)
        .await
        .is_err()
    {
        writer_task.abort();
        let _ = writer_task.await;
    }
    eprintln!(
        "[runtime-diagnostics] server stopped pid={}",
        std::process::id()
    );
    read_result?;
    Ok(())
}

struct ServerReply {
    value: Value,
    binary: Vec<Vec<u8>>,
}

impl ServerReply {
    fn json(value: Value) -> Self {
        Self {
            value,
            binary: Vec::new(),
        }
    }

    fn with_binary(value: Value, binary: Vec<Vec<u8>>) -> Self {
        Self { value, binary }
    }
}

async fn dispatch(
    state: &ServerState,
    method: &str,
    params: Value,
    binary: Vec<Vec<u8>>,
) -> Result<ServerReply, String> {
    if !matches!(
        method,
        "command.run" | "fs.write_file" | "chat.file_upload" | "terminal.write"
    ) && !binary.is_empty()
    {
        return Err(format!(
            "pilo-server method '{method}' does not accept binary attachments"
        ));
    }
    match method {
        "server.ping" => to_value(ServerPing {
            protocol_version: PROTOCOL_VERSION,
            server_version: SERVER_VERSION.to_owned(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            capabilities: SERVER_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect(),
        })
        .map(ServerReply::json),
        "server.status" => server_status(state).await.map(ServerReply::json),
        "environment.inspect" => environment_inspect(state, from_params(params)?)
            .await
            .map(ServerReply::json),
        "environment.pi_probe" => environment_pi_probe(state, from_params(params)?)
            .await
            .map(ServerReply::json),
        "command.run" => {
            command_run(state, from_params(params)?, one_binary(binary, method)?).await
        }
        "fs.read_dir" => blocking(move || fs_read_dir(from_params(params)?))
            .await
            .map(ServerReply::json),
        "fs.read_file" => {
            let data = tokio::task::spawn_blocking(move || fs_read_file(from_params(params)?))
                .await
                .map_err(|error| format!("pilo-server blocking task failed: {error}"))??;
            Ok(ServerReply::with_binary(Value::Null, vec![data]))
        }
        "fs.write_file" => {
            let data = one_binary(binary, method)?;
            blocking(move || fs_write_file(from_params(params)?, data))
                .await
                .map(ServerReply::json)
        }
        "chat.file_upload" => {
            let data = one_binary(binary, method)?;
            blocking(move || chat_files::upload(from_params(params)?, data))
                .await
                .map(ServerReply::json)
        }
        "chat.file_remove" => blocking(move || chat_files::remove(from_params(params)?))
            .await
            .map(ServerReply::json),
        "fs.stat" => blocking(move || fs_stat(from_params(params)?))
            .await
            .map(ServerReply::json),
        "fs.mkdir" => blocking(move || fs_mkdir(from_params(params)?))
            .await
            .map(ServerReply::json),
        "fs.mkdir_absolute" => blocking(move || fs_mkdir_absolute(from_params(params)?))
            .await
            .map(ServerReply::json),
        "fs.rename" => blocking(move || fs_rename(from_params(params)?))
            .await
            .map(ServerReply::json),
        "fs.remove" => blocking(move || fs_remove(from_params(params)?))
            .await
            .map(ServerReply::json),
        "fs.search" => blocking(move || fs_search(from_params(params)?))
            .await
            .map(ServerReply::json),
        "session.scan" => blocking(move || session_scan(from_params(params)?))
            .await
            .map(ServerReply::json),
        "session.activity" => {
            let owned_pids = state
                .pi
                .lock()
                .await
                .values()
                .filter_map(|process| process.child.id())
                .collect();
            blocking(move || session_activity(from_params(params)?, owned_pids))
                .await
                .map(ServerReply::json)
        }
        "session.search" => {
            let permit = Arc::clone(&state.search_slots)
                .acquire_owned()
                .await
                .map_err(|_| "session search is shutting down".to_owned())?;
            blocking(move || {
                // Keep the slot until the blocking scan ends, even if its caller goes away.
                let _permit = permit;
                session_search(from_params(params)?)
            })
            .await
            .map(ServerReply::json)
        }
        "session.read" => {
            let (metadata, data) =
                tokio::task::spawn_blocking(move || session_read(from_params(params)?))
                    .await
                    .map_err(|error| format!("pilo-server blocking task failed: {error}"))??;
            Ok(ServerReply::with_binary(metadata, vec![data]))
        }
        "session.delete" => blocking(move || session_delete(from_params(params)?))
            .await
            .map(ServerReply::json),
        "session.discover" => blocking(session_discover).await.map(ServerReply::json),
        "session.watch_start" => session_watch_start(state, from_params(params)?)
            .await
            .map(ServerReply::json),
        "session.watch_stop" => session_watch_stop(state, from_params(params)?)
            .await
            .map(ServerReply::json),
        "preview.ports" => preview_ports().await.map(ServerReply::json),
        "terminal.open" => terminal_open(state, from_params(params)?)
            .await
            .map(ServerReply::json),
        "terminal.write" => {
            terminal_write(state, from_params(params)?, one_binary(binary, method)?)
                .await
                .map(ServerReply::json)
        }
        "terminal.resize" => terminal_resize(state, from_params(params)?)
            .await
            .map(ServerReply::json),
        "terminal.close" => terminal_close(state, from_params(params)?)
            .await
            .map(ServerReply::json),
        "pi.start" => pi_start(state, from_params(params)?)
            .await
            .map(ServerReply::json),
        "pi.send" => pi_send(state, from_params(params)?)
            .await
            .map(ServerReply::json),
        "pi.stop" => pi_stop(state, from_params(params)?)
            .await
            .map(ServerReply::json),
        _ => Err(format!("unknown pilo-server method '{method}'")),
    }
}

fn from_params<T: for<'de> Deserialize<'de>>(params: Value) -> Result<T, String> {
    serde_json::from_value(params).map_err(|error| error.to_string())
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

fn one_binary(mut binary: Vec<Vec<u8>>, context: &str) -> Result<Vec<u8>, String> {
    if binary.len() != 1 {
        return Err(format!(
            "{context} expected exactly one binary attachment, got {}",
            binary.len()
        ));
    }
    let data = binary.remove(0);
    if data.len() > MAX_BINARY_PAYLOAD_BYTES {
        return Err(format!(
            "{context} binary attachment is {} bytes; pilo-server limit is {} bytes",
            data.len(),
            MAX_BINARY_PAYLOAD_BYTES
        ));
    }
    Ok(data)
}

async fn blocking<F>(operation: F) -> Result<Value, String>
where
    F: FnOnce() -> Result<Value, String> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| format!("pilo-server blocking task failed: {error}"))?
}

async fn server_status(state: &ServerState) -> Result<Value, String> {
    let pi_processes = state.pi.lock().await.len();
    let session_watchers = state.session_watchers.lock().await.len();
    let terminals = state
        .terminals
        .lock()
        .map_err(|_| "terminal registry is poisoned".to_owned())?
        .len();
    let uptime_ms = u64::try_from(state.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let available_requests = state.request_slots.available_permits();
    to_value(ServerStatus {
        protocol_version: PROTOCOL_VERSION,
        server_version: SERVER_VERSION.to_owned(),
        pid: std::process::id(),
        uptime_ms,
        active_requests: MAX_IN_FLIGHT_REQUESTS.saturating_sub(available_requests),
        max_in_flight_requests: MAX_IN_FLIGHT_REQUESTS,
        pi_processes,
        terminals,
        session_watchers,
    })
}

#[cfg(test)]
mod tests;
