use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, Response, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use context_relay::{
    secret_hash, secret_hash_matches, tenant_id_for_secret, valid_image_path,
    valid_registration_proof, valid_secret, valid_tenant_id, PollResponse, RegisterRequest,
    RelayResult, MAX_IMAGE_BYTES,
};
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use rustls::server::Acceptor;
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Mutex, Notify, RwLock, Semaphore};

const MAX_TENANTS: usize = 10_000;
const MAX_QUEUED_PER_TENANT: usize = 16;
const MAX_INFLIGHT: usize = 1_024;
const MAX_BLIND_SLOTS_PER_TENANT: usize = 8;
const MAX_BLIND_CONNECTIONS: usize = 1_024;
const MAX_CLIENT_HELLO_BYTES: usize = 64 * 1024;

struct Job {
    request_id: String,
    path_and_query: String,
}

struct Tenant {
    secret_hash: String,
    queue: Mutex<VecDeque<Job>>,
    notify: Notify,
}

struct Inflight {
    tenant_id: String,
    sender: oneshot::Sender<RelayResult>,
}

struct BlindConnection {
    stream: TcpStream,
    initial_bytes: Vec<u8>,
}

#[derive(Clone)]
struct AppState {
    tenants: Arc<RwLock<HashMap<String, Arc<Tenant>>>>,
    inflight: Arc<Mutex<HashMap<String, Inflight>>>,
    blind_slots: Arc<Mutex<HashMap<String, VecDeque<oneshot::Sender<BlindConnection>>>>>,
    blind_connections: Arc<Semaphore>,
    blind_suffix: Option<String>,
    tenant_store: Option<PathBuf>,
}

#[derive(Deserialize)]
struct ImageQuery {
    expires: Option<String>,
    sig: Option<String>,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("context-relay-server failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let listen = env::var("CONTEXT_RELAY_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let tenant_store = env::var("CONTEXT_RELAY_TENANT_STORE")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let blind_listen = env::var("CONTEXT_RELAY_BLIND_LISTEN")
        .ok()
        .filter(|value| !value.is_empty());
    let blind_suffix = env::var("CONTEXT_RELAY_BLIND_SUFFIX")
        .ok()
        .map(|value| value.trim_matches('.').to_ascii_lowercase())
        .filter(|value| valid_dns_suffix(value));
    if blind_listen.is_some() && blind_suffix.is_none() {
        return Err(
            "CONTEXT_RELAY_BLIND_SUFFIX is required when blind relay is enabled".to_string(),
        );
    }
    let state = AppState {
        tenants: Arc::new(RwLock::new(load_tenants(tenant_store.as_ref()).await?)),
        inflight: Arc::new(Mutex::new(HashMap::new())),
        blind_slots: Arc::new(Mutex::new(HashMap::new())),
        blind_connections: Arc::new(Semaphore::new(MAX_BLIND_CONNECTIONS)),
        blind_suffix,
        tenant_store,
    };
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route("/v1/register", post(register))
        .route("/v1/poll/{tenant_id}", post(poll))
        .route("/v1/result/{tenant_id}/{request_id}", post(result))
        .route("/v2/tunnel/{tenant_id}", get(blind_tunnel))
        .route("/t/{tenant_id}/image/{filename}", get(image))
        .layer(DefaultBodyLimit::max(MAX_IMAGE_BYTES * 2))
        .with_state(state.clone());
    let listener = TcpListener::bind(&listen)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(blind_listen) = blind_listen {
        let listener = TcpListener::bind(&blind_listen)
            .await
            .map_err(|error| format!("could not bind blind listener {blind_listen}: {error}"))?;
        let blind_state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = run_blind_listener(listener, blind_state).await {
                eprintln!("blind listener failed: {error}");
            }
        });
        println!("context-relay-server blind TLS listener on {blind_listen}");
    }
    println!("context-relay-server listening on {listen}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|error| error.to_string())
}

async fn blind_tunnel(
    Path(tenant_id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response<Body>, StatusCode> {
    if state.blind_suffix.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    authenticate(&state, &tenant_id, &headers).await?;
    Ok(upgrade
        .max_message_size(MAX_IMAGE_BYTES + MAX_CLIENT_HELLO_BYTES)
        .max_frame_size(64 * 1024)
        .on_upgrade(move |socket| blind_tunnel_socket(socket, tenant_id, state)))
}

async fn blind_tunnel_socket(mut socket: WebSocket, tenant_id: String, state: AppState) {
    let (sender, receiver) = oneshot::channel();
    {
        let mut slots = state.blind_slots.lock().await;
        let tenant_slots = slots.entry(tenant_id.clone()).or_default();
        if tenant_slots.len() >= MAX_BLIND_SLOTS_PER_TENANT {
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
        tenant_slots.push_back(sender);
    }
    let connection = tokio::time::timeout(Duration::from_secs(45), receiver).await;
    let Ok(Ok(connection)) = connection else {
        remove_closed_blind_slots(&state, &tenant_id).await;
        let _ = socket.send(Message::Close(None)).await;
        return;
    };
    if tokio::time::timeout(
        Duration::from_secs(60),
        bridge_websocket_to_tcp(socket, connection),
    )
    .await
    .is_err()
    {
        // Connection errors are deliberately not logged with tenant or request metadata.
    }
}

async fn remove_closed_blind_slots(state: &AppState, tenant_id: &str) {
    let mut slots = state.blind_slots.lock().await;
    if let Some(tenant_slots) = slots.get_mut(tenant_id) {
        tenant_slots.retain(|sender| !sender.is_closed());
        if tenant_slots.is_empty() {
            slots.remove(tenant_id);
        }
    }
}

async fn run_blind_listener(listener: TcpListener, state: AppState) -> Result<(), String> {
    loop {
        let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
        let permit = match state.blind_connections.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => continue,
        };
        let connection_state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = route_blind_connection(stream, connection_state).await;
        });
    }
}

async fn route_blind_connection(mut stream: TcpStream, state: AppState) -> Result<(), String> {
    let (server_name, initial_bytes) = read_client_hello(&mut stream).await?;
    let suffix = state
        .blind_suffix
        .as_deref()
        .ok_or_else(|| "blind relay disabled".to_string())?;
    let tenant_id = tenant_from_server_name(&server_name, suffix)
        .ok_or_else(|| "unroutable SNI".to_string())?;
    if !state.tenants.read().await.contains_key(&tenant_id) {
        return Err("unknown blind tenant".to_string());
    }
    let connection = BlindConnection {
        stream,
        initial_bytes,
    };
    let mut pending = Some(connection);
    let mut slots = state.blind_slots.lock().await;
    let Some(tenant_slots) = slots.get_mut(&tenant_id) else {
        return Err("no blind tunnel slot".to_string());
    };
    while let Some(sender) = tenant_slots.pop_front() {
        let connection = pending.take().expect("blind connection available");
        match sender.send(connection) {
            Ok(()) => {
                if tenant_slots.is_empty() {
                    slots.remove(&tenant_id);
                }
                return Ok(());
            }
            Err(connection) => pending = Some(connection),
        }
    }
    slots.remove(&tenant_id);
    Err("no live blind tunnel slot".to_string())
}

async fn read_client_hello(stream: &mut TcpStream) -> Result<(String, Vec<u8>), String> {
    let mut acceptor = Acceptor::default();
    let mut captured = Vec::new();
    let started = tokio::time::Instant::now();
    loop {
        if captured.len() >= MAX_CLIENT_HELLO_BYTES {
            return Err("TLS ClientHello exceeds limit".to_string());
        }
        let mut chunk = [0u8; 4096];
        let remaining = Duration::from_secs(5)
            .checked_sub(started.elapsed())
            .ok_or_else(|| "TLS ClientHello timed out".to_string())?;
        let read = tokio::time::timeout(remaining, stream.read(&mut chunk))
            .await
            .map_err(|_| "TLS ClientHello timed out".to_string())?
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("connection closed before TLS ClientHello".to_string());
        }
        captured.extend_from_slice(&chunk[..read]);
        let mut cursor = Cursor::new(&chunk[..read]);
        acceptor
            .read_tls(&mut cursor)
            .map_err(|error| error.to_string())?;
        match acceptor.accept() {
            Ok(Some(accepted)) => {
                let server_name = accepted
                    .client_hello()
                    .server_name()
                    .ok_or_else(|| "TLS ClientHello has no SNI".to_string())?
                    .to_ascii_lowercase();
                return Ok((server_name, captured));
            }
            Ok(None) => {}
            Err((error, _)) => return Err(error.to_string()),
        }
    }
}

async fn bridge_websocket_to_tcp(
    socket: WebSocket,
    connection: BlindConnection,
) -> Result<(), String> {
    let (mut websocket_sender, mut websocket_receiver) = socket.split();
    let (mut tcp_reader, mut tcp_writer) = connection.stream.into_split();
    websocket_sender
        .send(Message::Binary(connection.initial_bytes.into()))
        .await
        .map_err(|error| error.to_string())?;
    let websocket_to_tcp = async {
        while let Some(message) = websocket_receiver.next().await {
            match message.map_err(|error| error.to_string())? {
                Message::Binary(bytes) => tcp_writer
                    .write_all(&bytes)
                    .await
                    .map_err(|error| error.to_string())?,
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Text(_) => return Err("text frame on blind tunnel".to_string()),
            }
        }
        tcp_writer
            .shutdown()
            .await
            .map_err(|error| error.to_string())
    };
    let tcp_to_websocket = async {
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let read = tcp_reader
                .read(&mut buffer)
                .await
                .map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            websocket_sender
                .send(Message::Binary(buffer[..read].to_vec().into()))
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    };
    tokio::select! {
        result = websocket_to_tcp => result,
        result = tcp_to_websocket => result,
    }
}

fn valid_dns_suffix(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn tenant_from_server_name(server_name: &str, suffix: &str) -> Option<String> {
    let tenant = server_name.strip_suffix(&format!(".{suffix}"))?;
    (!tenant.contains('.') && valid_tenant_id(tenant)).then(|| tenant.to_string())
}

async fn register(State(state): State<AppState>, Json(input): Json<RegisterRequest>) -> StatusCode {
    if !valid_tenant_id(&input.tenant_id) || !valid_secret(&input.tenant_secret) {
        return StatusCode::BAD_REQUEST;
    }
    if tenant_id_for_secret(&input.tenant_secret) != input.tenant_id {
        return StatusCode::BAD_REQUEST;
    }
    if !valid_registration_proof(
        &input.tenant_id,
        &input.tenant_secret,
        input.registration_nonce,
    ) {
        return StatusCode::BAD_REQUEST;
    }
    let hash = secret_hash(&input.tenant_secret);
    let mut tenants = state.tenants.write().await;
    if let Some(existing) = tenants.get(&input.tenant_id) {
        return if secret_hash_matches(&existing.secret_hash, &input.tenant_secret) {
            StatusCode::NO_CONTENT
        } else {
            StatusCode::CONFLICT
        };
    }
    if tenants.len() >= MAX_TENANTS {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    tenants.insert(
        input.tenant_id.clone(),
        Arc::new(Tenant {
            secret_hash: hash,
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
        }),
    );
    if persist_tenants(state.tenant_store.as_ref(), &tenants)
        .await
        .is_err()
    {
        tenants.remove(&input.tenant_id);
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::CREATED
}

async fn poll(
    Path(tenant_id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PollResponse>, StatusCode> {
    let tenant = authenticate(&state, &tenant_id, &headers).await?;
    for _ in 0..2 {
        if let Some(job) = tenant.queue.lock().await.pop_front() {
            return Ok(Json(PollResponse {
                request_id: job.request_id,
                path_and_query: job.path_and_query,
            }));
        }
        tokio::time::timeout(Duration::from_secs(20), tenant.notify.notified())
            .await
            .ok();
    }
    Err(StatusCode::NO_CONTENT)
}

async fn result(
    Path((tenant_id, request_id)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(result): Json<RelayResult>,
) -> StatusCode {
    if authenticate(&state, &tenant_id, &headers).await.is_err()
        || result.body_base64.len() > MAX_IMAGE_BYTES * 2
    {
        return StatusCode::NOT_FOUND;
    }
    let mut inflight_map = state.inflight.lock().await;
    let Some(inflight) = inflight_map.get(&request_id) else {
        return StatusCode::NOT_FOUND;
    };
    if inflight.tenant_id != tenant_id {
        return StatusCode::NOT_FOUND;
    }
    let Some(inflight) = inflight_map.remove(&request_id) else {
        return StatusCode::NOT_FOUND;
    };
    drop(inflight_map);
    let _ = inflight.sender.send(result);
    StatusCode::NO_CONTENT
}

async fn image(
    Path((tenant_id, filename)): Path<(String, String)>,
    Query(query): Query<ImageQuery>,
    State(state): State<AppState>,
) -> Response<Body> {
    let Some(expires) = query.expires else {
        return hidden_not_found();
    };
    let Some(sig) = query.sig else {
        return hidden_not_found();
    };
    if expires.len() > 20
        || !expires.bytes().all(|byte| byte.is_ascii_digit())
        || sig.len() != 64
        || !sig.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return hidden_not_found();
    }
    let path_and_query = format!("/image/{filename}?expires={expires}&sig={sig}");
    if !valid_tenant_id(&tenant_id) || !valid_image_path(&path_and_query) {
        return hidden_not_found();
    }
    let Some(tenant) = state.tenants.read().await.get(&tenant_id).cloned() else {
        return hidden_not_found();
    };
    let request_id = random_hex(16);
    let (sender, receiver) = oneshot::channel();
    let mut inflight = state.inflight.lock().await;
    if inflight.len() >= MAX_INFLIGHT {
        return hidden_not_found();
    }
    let mut queue = tenant.queue.lock().await;
    if queue.len() >= MAX_QUEUED_PER_TENANT {
        return hidden_not_found();
    }
    inflight.insert(
        request_id.clone(),
        Inflight {
            tenant_id: tenant_id.clone(),
            sender,
        },
    );
    queue.push_back(Job {
        request_id: request_id.clone(),
        path_and_query,
    });
    drop(queue);
    drop(inflight);
    tenant.notify.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(30), receiver).await;
    state.inflight.lock().await.remove(&request_id);
    tenant
        .queue
        .lock()
        .await
        .retain(|job| job.request_id != request_id);
    let Ok(Ok(result)) = result else {
        return hidden_not_found();
    };
    let Ok(body) = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        result.body_base64,
    ) else {
        return hidden_not_found();
    };
    if body.len() > MAX_IMAGE_BYTES
        || result.status != 200
        || !result.content_type.starts_with("image/")
    {
        return hidden_not_found();
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, result.content_type)
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(body))
        .unwrap_or_else(|_| hidden_not_found())
}

async fn authenticate(
    state: &AppState,
    tenant_id: &str,
    headers: &HeaderMap,
) -> Result<Arc<Tenant>, StatusCode> {
    let secret = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(StatusCode::NOT_FOUND)?;
    let tenant = state
        .tenants
        .read()
        .await
        .get(tenant_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;
    if !secret_hash_matches(&tenant.secret_hash, secret) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(tenant)
}

async fn load_tenants(path: Option<&PathBuf>) -> Result<HashMap<String, Arc<Tenant>>, String> {
    let Some(path) = path else {
        return Ok(HashMap::new());
    };
    let Ok(bytes) = tokio::fs::read(path).await else {
        return Ok(HashMap::new());
    };
    let stored: HashMap<String, String> =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    Ok(stored
        .into_iter()
        .map(|(id, hash)| {
            (
                id,
                Arc::new(Tenant {
                    secret_hash: hash,
                    queue: Mutex::new(VecDeque::new()),
                    notify: Notify::new(),
                }),
            )
        })
        .collect())
}

async fn persist_tenants(
    path: Option<&PathBuf>,
    tenants: &HashMap<String, Arc<Tenant>>,
) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    let stored: HashMap<&String, &String> = tenants
        .iter()
        .map(|(id, tenant)| (id, &tenant.secret_hash))
        .collect();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec(&stored).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    tokio::fs::write(&temporary, bytes)
        .await
        .map_err(|error| error.to_string())?;
    set_private_permissions(&temporary)?;
    tokio::fs::rename(temporary, path)
        .await
        .map_err(|error| error.to_string())
}

fn set_private_permissions(path: &std::path::Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn hidden_not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::empty())
        .unwrap()
}

fn random_hex(bytes: usize) -> String {
    let mut value = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut value);
    hex::encode(value)
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use context_relay::secret_hash;
    use futures_util::{SinkExt, StreamExt};
    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, ClientConnection, RootCertStore};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::{header, HeaderValue};
    use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;

    fn make_client_hello(hostname: String) -> Vec<u8> {
        let provider = rustls::crypto::ring::default_provider();
        let config = ClientConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(RootCertStore::empty())
            .with_no_client_auth();
        let server_name = ServerName::try_from(hostname).unwrap();
        let mut connection = ClientConnection::new(Arc::new(config), server_name).unwrap();
        let mut client_hello = Vec::new();
        connection.write_tls(&mut client_hello).unwrap();
        client_hello
    }

    #[test]
    fn maps_only_exact_tenant_sni() {
        let tenant = "a".repeat(32);
        let suffix = "relay.example.com";
        assert_eq!(
            tenant_from_server_name(&format!("{tenant}.{suffix}"), suffix),
            Some(tenant.clone())
        );
        assert!(tenant_from_server_name(&format!("x.{tenant}.{suffix}"), suffix).is_none());
        assert!(tenant_from_server_name("relay.example.com", suffix).is_none());
        assert!(tenant_from_server_name(&format!("{}.other.example", tenant), suffix).is_none());
    }

    #[tokio::test]
    async fn reads_sni_without_terminating_tls() {
        let tenant = "b".repeat(32);
        let hostname = format!("{tenant}.relay.example.com");
        let client_hello = make_client_hello(hostname.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let sender = tokio::spawn(async move {
            let mut stream = TcpStream::connect(address).await.unwrap();
            stream.write_all(&client_hello).await.unwrap();
        });
        let (mut stream, _) = listener.accept().await.unwrap();
        let (parsed, captured) = read_client_hello(&mut stream).await.unwrap();
        sender.await.unwrap();
        assert_eq!(parsed, hostname);
        assert!(!captured.is_empty());
    }

    #[tokio::test]
    async fn blind_relay_routes_opaque_bytes_both_directions() {
        let secret = "c".repeat(64);
        let tenant_id = tenant_id_for_secret(&secret);
        let suffix = "relay.example.com";
        let tenant = Arc::new(Tenant {
            secret_hash: secret_hash(&secret),
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
        });
        let state = AppState {
            tenants: Arc::new(RwLock::new(HashMap::from([(tenant_id.clone(), tenant)]))),
            inflight: Arc::new(Mutex::new(HashMap::new())),
            blind_slots: Arc::new(Mutex::new(HashMap::new())),
            blind_connections: Arc::new(Semaphore::new(MAX_BLIND_CONNECTIONS)),
            blind_suffix: Some(suffix.to_string()),
            tenant_store: None,
        };
        let control_app = Router::new()
            .route("/v2/tunnel/{tenant_id}", get(blind_tunnel))
            .with_state(state.clone());
        let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let control_address = control_listener.local_addr().unwrap();
        let control_task = tokio::spawn(async move {
            axum::serve(control_listener, control_app).await.unwrap();
        });
        let blind_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let blind_address = blind_listener.local_addr().unwrap();
        let blind_task = tokio::spawn(run_blind_listener(blind_listener, state));

        let mut request = format!("ws://{control_address}/v2/tunnel/{tenant_id}")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
        );
        let (mut tunnel, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;

        let hostname = format!("{tenant_id}.{suffix}");
        let client_hello = make_client_hello(hostname);
        let mut public = TcpStream::connect(blind_address).await.unwrap();
        public.write_all(&client_hello).await.unwrap();
        let assigned = tunnel.next().await.unwrap().unwrap();
        assert_eq!(assigned.into_data(), client_hello);

        let reply = b"opaque-inner-tls-response";
        tunnel
            .send(TungsteniteMessage::Binary(reply.to_vec().into()))
            .await
            .unwrap();
        let mut received = vec![0u8; reply.len()];
        public.read_exact(&mut received).await.unwrap();
        assert_eq!(received, reply);

        control_task.abort();
        blind_task.abort();
    }
}
