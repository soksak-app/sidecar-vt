//! in-process terminal service를 위한 persistent protocol-1 transport이다.

use crate::protocol::{serve_with_registry, LocalSessionPort, PersistentOwner, PersistentRegistry};
use nix::errno::Errno;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};
use tokio::task::JoinSet;
use tokio::time::{timeout, Duration};
use uuid::Uuid;

pub const PROTOCOL: u64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub protocol: u64,
    pub pid: u32,
    pub socket: String,
    pub token: String,
}

#[derive(Debug, Deserialize)]
struct Hello {
    operation: String,
    protocol: u64,
    token: String,
    client: String,
}

#[derive(Debug, Serialize)]
struct HelloReply<'a> {
    operation: &'static str,
    protocol: Option<u64>,
    /// The version of this sidecar, by which a host tells a service of another version than the installed one
    /// (core docs/spec/terminal-runtime.md).
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<&'static str>,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

pub struct AlreadyRunning {
    pub endpoint: Option<Endpoint>,
}

struct ServiceCleanup {
    service_dir: PathBuf,
    socket_dir: PathBuf,
}

impl Drop for ServiceCleanup {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(self.service_dir.join("endpoint.json")) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("service endpoint cleanup failed: {error}");
            }
        }
        if let Err(error) = fs::remove_dir_all(&self.socket_dir) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("service socket cleanup failed: {error}");
            }
        }
    }
}

struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        if let Err(error) = nix::fcntl::flock(self.0.as_raw_fd(), nix::fcntl::FlockArg::Unlock) {
            eprintln!("service lock unlock failed: {error}");
        }
    }
}

fn service_lock(service_dir: &Path) -> Result<File, String> {
    fs::create_dir_all(service_dir).map_err(|e| format!("create service directory: {e}"))?;
    fs::set_permissions(service_dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("secure service directory: {e}"))?;
    let path = service_dir.join("service.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("open service lock: {e}"))?;
    match nix::fcntl::flock(
        file.as_raw_fd(),
        nix::fcntl::FlockArg::LockExclusiveNonblock,
    ) {
        Ok(_) => Ok(file),
        Err(error) if error == Errno::EAGAIN || error == Errno::EWOULDBLOCK => {
            Err("already-running".to_string())
        }
        Err(error) => Err(format!("lock service: {error}")),
    }
}

fn write_endpoint(service_dir: &Path, endpoint: &Endpoint) -> Result<(), String> {
    let bytes = serde_json::to_vec(endpoint).map_err(|e| format!("encode endpoint: {e}"))?;
    let temporary = service_dir.join(format!("endpoint.json.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| format!("create endpoint: {e}"))?;
    file.write_all(&bytes)
        .map_err(|e| format!("write endpoint: {e}"))?;
    file.sync_all().map_err(|e| format!("sync endpoint: {e}"))?;
    fs::rename(temporary, service_dir.join("endpoint.json"))
        .map_err(|e| format!("publish endpoint: {e}"))
}

fn read_endpoint(service_dir: &Path) -> Result<Endpoint, String> {
    let path = service_dir.join("endpoint.json");
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("decode {}: {error}", path.display()))
}

/// 잠금을 얻었으면 이전 service 는 끝났다. 강제 종료나 충돌로 정리하지 못하고 끝난 이전 service 의 socket 디렉터리를
/// 지운다. 이 service 가 만드는 /tmp/spv-* 디렉터리만 지우며, 지우지 못하면 그 까닭을 보고한다.
fn remove_previous_socket(service_dir: &Path) {
    if !service_dir.join("endpoint.json").exists() {
        return;
    }
    let endpoint = match read_endpoint(service_dir) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            eprintln!("previous service endpoint: {error}");
            return;
        }
    };
    let socket = PathBuf::from(&endpoint.socket);
    let owned = socket.parent().filter(|directory| {
        directory.parent() == Some(Path::new("/tmp"))
            && directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("spv-"))
    });
    let Some(directory) = owned else {
        eprintln!(
            "previous service socket {} is not in a service socket directory",
            endpoint.socket
        );
        return;
    };
    if let Err(error) = fs::remove_dir_all(directory) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!("previous service socket cleanup failed: {error}");
        }
    }
}

fn socket_path() -> Result<(PathBuf, PathBuf), String> {
    // 절대 socket 경로를 macOS의 104-byte Unix socket 한도 미만으로 유지한다.
    let directory = PathBuf::from("/tmp").join(format!("spv-{}", Uuid::new_v4()));
    fs::create_dir(&directory).map_err(|e| format!("create socket directory: {e}"))?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("secure socket directory: {e}"))?;
    Ok((directory.clone(), directory.join("service.sock")))
}

async fn authenticate(
    stream: UnixStream,
    expected_token: &str,
) -> Result<
    (
        BufReader<tokio::net::unix::OwnedReadHalf>,
        tokio::net::unix::OwnedWriteHalf,
        String,
    ),
    String,
> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .map_err(|_| "hello timeout".to_string())?
        .map_err(|e| format!("read hello: {e}"))?;
    let hello =
        serde_json::from_str::<Hello>(line.trim()).map_err(|_| "invalid hello".to_string())?;
    let valid = hello.operation == "hello"
        && hello.protocol == PROTOCOL
        && hello.token == expected_token
        && !hello.client.is_empty();
    let reply = if valid {
        HelloReply {
            operation: "hello",
            protocol: Some(PROTOCOL),
            // Both crates and vt-alacritty/package.json declare the same version (tests/versions.test.mjs).
            version: Some(env!("CARGO_PKG_VERSION")),
            ok: true,
            error: None,
        }
    } else {
        HelloReply {
            operation: "hello",
            protocol: None,
            version: None,
            ok: false,
            error: Some("authentication or protocol mismatch"),
        }
    };
    let mut encoded = serde_json::to_vec(&reply).map_err(|e| format!("encode hello reply: {e}"))?;
    encoded.push(b'\n');
    write_half
        .write_all(&encoded)
        .await
        .map_err(|e| format!("write hello reply: {e}"))?;
    write_half
        .flush()
        .await
        .map_err(|e| format!("flush hello reply: {e}"))?;
    if !valid {
        return Err("authentication or protocol mismatch".to_string());
    }
    Ok((reader, write_half, hello.client))
}

/// persistent service를 실행한다.  PTY service는 수락된 모든 connection이
/// 공유하므로, transport 연결이 끊겨도 그 session은 멈추지 않는다.
pub async fn serve_persistent(
    service_dir: &Path,
    engine_factory: Arc<dyn Fn() -> Box<dyn crate::protocol::Engine> + Send + Sync>,
) -> Result<(), String> {
    crate::platform::darwin::frame::load_default_font()?;
    let _lock = match service_lock(service_dir) {
        Ok(file) => Lock(file),
        Err(error) if error == "already-running" => {
            let endpoint = read_endpoint(service_dir)?;
            let mut line = serde_json::to_vec(
                &serde_json::json!({"error":"already-running","endpoint":endpoint}),
            )
            .map_err(|e| e.to_string())?;
            line.push(b'\n');
            std::io::stdout()
                .write_all(&line)
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    remove_previous_socket(service_dir);
    let (socket_dir, socket) = socket_path()?;
    let listener = UnixListener::bind(&socket).map_err(|e| format!("bind service socket: {e}"))?;
    let _cleanup = ServiceCleanup {
        service_dir: service_dir.to_path_buf(),
        socket_dir,
    };
    let token = Uuid::new_v4().to_string();
    let endpoint = Endpoint {
        protocol: PROTOCOL,
        pid: std::process::id(),
        socket: socket.to_string_lossy().into_owned(),
        token,
    };
    // 종료 요청을 받으면 루프를 끝내 endpoint 와 socket 디렉터리를 정리한다. 세션의 셸은 PTY 가 닫히면 끝난다. 처리기는
    // 준비를 알리기 전에 둔다. 준비를 본 쪽이 곧바로 보낸 종료 요청이 기본 동작으로 process 를 끝내면 정리가 실행되지 않는다.
    let mut terminate =
        signal(SignalKind::terminate()).map_err(|e| format!("terminate signal: {e}"))?;
    let mut interrupt =
        signal(SignalKind::interrupt()).map_err(|e| format!("interrupt signal: {e}"))?;
    write_endpoint(service_dir, &endpoint)?;
    let mut ready =
        serde_json::to_vec(&endpoint).map_err(|e| format!("encode ready endpoint: {e}"))?;
    ready.push(b'\n');
    std::io::stdout()
        .write_all(&ready)
        .map_err(|e| format!("write ready endpoint: {e}"))?;
    std::io::stdout()
        .flush()
        .map_err(|e| format!("flush ready endpoint: {e}"))?;

    // 성능 트레이스(V5-104). 플래그가 없으면 아래 두 호출은 아무 파일 작업도 하지 않는다.
    let performance = crate::performance::PerformanceTrace::from_service_dir(service_dir);
    performance.line("session_start", serde_json::json!({"role": "vt-core"}));
    let service = Arc::new(crate::pty::PtyService::new().with_performance(performance.clone()));
    let registry = PersistentRegistry::new();
    let mut clients = JoinSet::new();
    let mut had_client = false;
    loop {
        tokio::select! {
            _ = terminate.recv() => return Ok(()),
            _ = interrupt.recv() => return Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|e| format!("accept service client: {e}"))?;
                had_client = true;
                let token = endpoint.token.clone();
                let service = Arc::clone(&service);
                let engine_factory = Arc::clone(&engine_factory);
                let registry = Arc::clone(&registry);
                // The trace of each connection reads the flag of the service directory at each event
                // (core docs/spec/performance-trace.md).
                let trace_dir = service_dir.to_path_buf();
                clients.spawn(async move {
            if let Ok((reader, writer, client)) = authenticate(stream, &token).await {
                crate::performance::PerformanceTrace::from_service_dir(&trace_dir)
                    .line("client_connect", serde_json::json!({"owner": client}));
                let owner = client.clone();
                let factory_service = Arc::clone(&service);
                let factory = Arc::new(move || {
                    Arc::new(LocalSessionPort::new_with_owner(
                        Arc::clone(&factory_service),
                        owner.clone(),
                    )) as Arc<dyn crate::protocol::SessionPort>
                });
                let close_service = Arc::clone(&service);
                let close_client = client.clone();
                let close_owner = Arc::new(move || close_service.close_owner(&close_client));
                // The trace of the connection reaches the surface actors.
                let connection_trace =
                    crate::performance::PerformanceTrace::from_service_dir(&trace_dir);
                if let Err(error) = serve_with_registry(
                    engine_factory,
                    reader,
                    writer,
                    factory,
                    PersistentOwner {
                        close: close_owner,
                        registry,
                        owner: client,
                        performance: connection_trace,
                    },
                )
                .await
                {
                    eprintln!("sidecar client session failed: {error}");
                }
            }
                });
            }
            Some(_) = clients.join_next(), if had_client => {
                if clients.is_empty() && registry.shutdown_requested() {
                    return Ok(());
                }
            }
        }
    }
}

pub fn canonical_config_dir(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|e| format!("canonicalize config directory: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn endpoint_is_atomic_and_private() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path()).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = Endpoint {
            protocol: PROTOCOL,
            pid: 123,
            socket: "/tmp/service.sock".to_string(),
            token: "random-token".to_string(),
        };
        write_endpoint(directory.path(), &endpoint).unwrap();
        assert_eq!(read_endpoint(directory.path()).unwrap().protocol, PROTOCOL);
        let mode = fs::metadata(directory.path().join("endpoint.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn lock_is_single_instance_and_releases() {
        let directory = tempfile::tempdir().unwrap();
        let first = service_lock(directory.path()).unwrap();
        assert_eq!(
            service_lock(directory.path()).unwrap_err(),
            "already-running"
        );
        drop(first);
        assert!(service_lock(directory.path()).is_ok());
    }

    #[test]
    fn malformed_endpoint_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("endpoint.json"), b"not-json").unwrap();
        let error = read_endpoint(directory.path()).unwrap_err();
        assert!(error.contains("decode"));
    }

    #[tokio::test]
    async fn hello_authenticates_before_protocol_frames() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(async move { authenticate(server, "token").await });
        client
            .write_all(
                br#"{"operation":"hello","protocol":1,"token":"token","client":"config-a"}
"#,
            )
            .await
            .unwrap();
        let mut reply = String::new();
        BufReader::new(client).read_line(&mut reply).await.unwrap();
        let reply = serde_json::from_str::<serde_json::Value>(reply.trim()).unwrap();
        assert_eq!(reply["ok"], true);
        // The reply carries the version of this sidecar, so a host can tell a service of another version.
        assert_eq!(
            reply["version"],
            env!("CARGO_PKG_VERSION"),
            "hello reply {reply}"
        );
        let (_, _, client_id) = task.await.unwrap().unwrap();
        assert_eq!(client_id, "config-a");
    }

    #[tokio::test]
    async fn hello_rejects_wrong_token() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(async move { authenticate(server, "token").await });
        client
            .write_all(
                br#"{"operation":"hello","protocol":1,"token":"wrong","client":"config-a"}
"#,
            )
            .await
            .unwrap();
        let mut reply = String::new();
        BufReader::new(client).read_line(&mut reply).await.unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(reply.trim()).unwrap()["ok"],
            false
        );
        assert!(task.await.unwrap().is_err());
    }
}
