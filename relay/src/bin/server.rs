use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, Response, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use context_relay::{
    secret_hash, secret_hash_matches, tenant_id_for_secret, valid_image_path,
    valid_registration_proof, valid_secret, valid_tenant_id, PollResponse, RegisterRequest,
    RelayResult, MAX_IMAGE_BYTES,
};
use rand::RngCore;
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, Mutex, Notify, RwLock};

const MAX_TENANTS: usize = 10_000;
const MAX_QUEUED_PER_TENANT: usize = 16;
const MAX_INFLIGHT: usize = 1_024;

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

#[derive(Clone)]
struct AppState {
    tenants: Arc<RwLock<HashMap<String, Arc<Tenant>>>>,
    inflight: Arc<Mutex<HashMap<String, Inflight>>>,
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
    let state = AppState {
        tenants: Arc::new(RwLock::new(load_tenants(tenant_store.as_ref()).await?)),
        inflight: Arc::new(Mutex::new(HashMap::new())),
        tenant_store,
    };
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route("/v1/register", post(register))
        .route("/v1/poll/{tenant_id}", post(poll))
        .route("/v1/result/{tenant_id}/{request_id}", post(result))
        .route("/t/{tenant_id}/image/{filename}", get(image))
        .layer(DefaultBodyLimit::max(MAX_IMAGE_BYTES * 2))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .map_err(|error| error.to_string())?;
    println!("context-relay-server listening on {listen}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|error| error.to_string())
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
