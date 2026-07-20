use base64::Engine;
use context_relay::{ClientIdentity, PollResponse, RegisterRequest, RelayResult, MAX_IMAGE_BYTES};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, StatusCode};
use std::env;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{header, HeaderValue};
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("context-relay-client failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let initialize_only = env::args().any(|argument| argument == "--init");
    let relay_url =
        env::var("CONTEXT_RELAY_URL").map_err(|_| "CONTEXT_RELAY_URL is required".to_string())?;
    if !relay_url.starts_with("https://") {
        return Err("CONTEXT_RELAY_URL must use https://".to_string());
    }
    let local_gateway =
        env::var("CONTEXT_RELAY_LOCAL_GATEWAY").unwrap_or_else(|_| "http://[::1]:8787".to_string());
    if local_gateway != "http://[::1]:8787" && local_gateway != "http://127.0.0.1:8787" {
        return Err("local gateway must be loopback port 8787".to_string());
    }
    let identity_file = PathBuf::from(
        env::var("CONTEXT_RELAY_IDENTITY_FILE").unwrap_or_else(|_| default_identity_file()),
    );
    let identity = load_or_create_identity(&identity_file).await?;
    if initialize_only {
        println!("{}", identity.tenant_id);
        return Ok(());
    }
    let relay_client = Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|error| error.to_string())?;
    let local_client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    register(&relay_client, &relay_url, &identity).await?;
    println!("context-relay-client registered");
    let blind_gateway = env::var("CONTEXT_RELAY_BLIND_GATEWAY")
        .ok()
        .filter(|value| !value.is_empty());
    if let Some(gateway) = blind_gateway.as_deref() {
        validate_blind_gateway(gateway)?;
        let slots = env::var("CONTEXT_RELAY_BLIND_SLOTS")
            .unwrap_or_else(|_| "4".to_string())
            .parse::<usize>()
            .map_err(|_| "CONTEXT_RELAY_BLIND_SLOTS must be an integer".to_string())?;
        if !(1..=8).contains(&slots) {
            return Err("CONTEXT_RELAY_BLIND_SLOTS must be between 1 and 8".to_string());
        }
        for _ in 0..slots {
            let tunnel_url = blind_tunnel_url(&relay_url, &identity.tenant_id)?;
            let gateway = gateway.to_string();
            let secret = identity.tenant_secret.clone();
            tokio::spawn(async move {
                blind_slot_loop(tunnel_url, gateway, secret).await;
            });
        }
        println!("context-relay-client blind TLS slots={slots} gateway={gateway}");
    }
    if blind_gateway.is_some() && env_flag("CONTEXT_RELAY_BLIND_ONLY") {
        tokio::signal::ctrl_c()
            .await
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    loop {
        match poll_once(
            &relay_client,
            &local_client,
            &relay_url,
            &local_gateway,
            &identity,
        )
        .await
        {
            Ok(()) => {}
            Err(PollError::Unregistered) => {
                register(&relay_client, &relay_url, &identity).await?;
            }
            Err(PollError::Other(error)) => {
                eprintln!("relay poll failed: {error}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

fn validate_blind_gateway(value: &str) -> Result<(), String> {
    let address = value
        .parse::<SocketAddr>()
        .map_err(|_| "CONTEXT_RELAY_BLIND_GATEWAY must be an IP socket address".to_string())?;
    if !address.ip().is_loopback() {
        return Err("blind TLS gateway must use a loopback address".to_string());
    }
    Ok(())
}

fn blind_tunnel_url(relay_url: &str, tenant_id: &str) -> Result<String, String> {
    let base = relay_url
        .strip_prefix("https://")
        .ok_or_else(|| "blind tunnel control URL must use HTTPS".to_string())?;
    Ok(format!(
        "wss://{}/v2/tunnel/{tenant_id}",
        base.trim_end_matches('/')
    ))
}

async fn blind_slot_loop(tunnel_url: String, gateway: String, secret: String) {
    loop {
        if run_blind_slot(&tunnel_url, &gateway, &secret)
            .await
            .is_err()
        {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

async fn run_blind_slot(tunnel_url: &str, gateway: &str, secret: &str) -> Result<(), String> {
    let mut request = tunnel_url
        .into_client_request()
        .map_err(|error| error.to_string())?;
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {secret}")).map_err(|error| error.to_string())?,
    );
    let (mut websocket, response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("blind tunnel returned {}", response.status()));
    }
    let first_bytes = loop {
        let message = websocket
            .next()
            .await
            .ok_or_else(|| "blind tunnel closed before assignment".to_string())?
            .map_err(|error| error.to_string())?;
        match message {
            Message::Binary(bytes) => break bytes,
            Message::Ping(bytes) => websocket
                .send(Message::Pong(bytes))
                .await
                .map_err(|error| error.to_string())?,
            Message::Pong(_) => {}
            Message::Close(_) => return Err("blind tunnel closed before assignment".to_string()),
            Message::Text(_) | Message::Frame(_) => {
                return Err("unexpected frame on blind tunnel".to_string())
            }
        }
    };
    let mut local = TcpStream::connect(gateway)
        .await
        .map_err(|error| error.to_string())?;
    local
        .write_all(&first_bytes)
        .await
        .map_err(|error| error.to_string())?;
    bridge_local_to_websocket(websocket, local).await
}

async fn bridge_local_to_websocket(
    websocket: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    local: TcpStream,
) -> Result<(), String> {
    let (mut websocket_sender, mut websocket_receiver) = websocket.split();
    let (mut local_reader, mut local_writer) = local.into_split();
    let websocket_to_local = async {
        while let Some(message) = websocket_receiver.next().await {
            match message.map_err(|error| error.to_string())? {
                Message::Binary(bytes) => local_writer
                    .write_all(&bytes)
                    .await
                    .map_err(|error| error.to_string())?,
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Text(_) | Message::Frame(_) => {
                    return Err("unexpected frame on blind tunnel".to_string())
                }
            }
        }
        local_writer
            .shutdown()
            .await
            .map_err(|error| error.to_string())
    };
    let local_to_websocket = async {
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let read = local_reader
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
        result = websocket_to_local => result,
        result = local_to_websocket => result,
    }
}

async fn register(
    client: &Client,
    relay_url: &str,
    identity: &ClientIdentity,
) -> Result<(), String> {
    let response = client
        .post(format!("{}/v1/register", relay_url.trim_end_matches('/')))
        .json(&RegisterRequest {
            tenant_id: identity.tenant_id.clone(),
            tenant_secret: identity.tenant_secret.clone(),
            registration_nonce: identity.registration_nonce,
        })
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("registration returned {}", response.status()))
    }
}

async fn poll_once(
    relay_client: &Client,
    local_client: &Client,
    relay_url: &str,
    local_gateway: &str,
    identity: &ClientIdentity,
) -> Result<(), PollError> {
    let base = relay_url.trim_end_matches('/');
    let response = relay_client
        .post(format!("{base}/v1/poll/{}", identity.tenant_id))
        .bearer_auth(&identity.tenant_secret)
        .send()
        .await
        .map_err(|error| PollError::Other(error.to_string()))?;
    if response.status() == StatusCode::NO_CONTENT {
        return Ok(());
    }
    if response.status() == StatusCode::NOT_FOUND {
        return Err(PollError::Unregistered);
    }
    if !response.status().is_success() {
        return Err(PollError::Other(format!(
            "poll returned {}",
            response.status()
        )));
    }
    let job: PollResponse = response
        .json()
        .await
        .map_err(|error| PollError::Other(error.to_string()))?;
    let local = local_client
        .get(format!(
            "{}{}",
            local_gateway.trim_end_matches('/'),
            job.path_and_query
        ))
        .send()
        .await
        .map_err(|error| PollError::Other(error.to_string()))?;
    let status = local.status().as_u16();
    let content_type = local
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let body = local
        .bytes()
        .await
        .map_err(|error| PollError::Other(error.to_string()))?;
    if body.len() > MAX_IMAGE_BYTES {
        return Err(PollError::Other(
            "local image exceeds size limit".to_string(),
        ));
    }
    let result = RelayResult {
        status,
        content_type,
        body_base64: base64::engine::general_purpose::STANDARD.encode(body),
    };
    let response = relay_client
        .post(format!(
            "{base}/v1/result/{}/{}",
            identity.tenant_id, job.request_id
        ))
        .bearer_auth(&identity.tenant_secret)
        .json(&result)
        .send()
        .await
        .map_err(|error| PollError::Other(error.to_string()))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(PollError::Other(format!(
            "result returned {}",
            response.status()
        )))
    }
}

enum PollError {
    Unregistered,
    Other(String),
}

async fn load_or_create_identity(path: &Path) -> Result<ClientIdentity, String> {
    if let Ok(bytes) = tokio::fs::read(path).await {
        let identity: ClientIdentity =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if validate_identity(&identity).is_err()
            && context_relay::valid_secret(&identity.tenant_secret)
        {
            let migrated = ClientIdentity::from_secret(identity.tenant_secret);
            replace_identity(path, &migrated).await?;
            return Ok(migrated);
        }
        validate_identity(&identity)?;
        set_private_permissions(path)?;
        return Ok(identity);
    }
    let identity = ClientIdentity::generate();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec(&identity).map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    match options.open(&temporary).await {
        Ok(mut file) => {
            use tokio::io::AsyncWriteExt;
            file.write_all(&bytes)
                .await
                .map_err(|error| error.to_string())?;
            file.sync_all().await.map_err(|error| error.to_string())?;
            drop(file);
            match tokio::fs::hard_link(&temporary, path).await {
                Ok(()) => {
                    let _ = tokio::fs::remove_file(&temporary).await;
                    Ok(identity)
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    let _ = tokio::fs::remove_file(&temporary).await;
                    let existing = tokio::fs::read(path)
                        .await
                        .map_err(|read_error| read_error.to_string())?;
                    let identity: ClientIdentity = serde_json::from_slice(&existing)
                        .map_err(|parse_error| parse_error.to_string())?;
                    validate_identity(&identity)?;
                    set_private_permissions(path)?;
                    Ok(identity)
                }
                Err(error) => {
                    let _ = tokio::fs::remove_file(&temporary).await;
                    Err(error.to_string())
                }
            }
        }
        Err(error) => Err(error.to_string()),
    }
}

async fn replace_identity(path: &Path, identity: &ClientIdentity) -> Result<(), String> {
    let bytes = serde_json::to_vec(identity).map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("{}.replace", std::process::id()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .await
        .map_err(|error| error.to_string())?;
    use tokio::io::AsyncWriteExt;
    file.write_all(&bytes)
        .await
        .map_err(|error| error.to_string())?;
    file.sync_all().await.map_err(|error| error.to_string())?;
    drop(file);
    tokio::fs::rename(&temporary, path)
        .await
        .map_err(|error| error.to_string())?;
    set_private_permissions(path)
}

fn validate_identity(identity: &ClientIdentity) -> Result<(), String> {
    if !context_relay::valid_tenant_id(&identity.tenant_id)
        || !context_relay::valid_secret(&identity.tenant_secret)
        || context_relay::tenant_id_for_secret(&identity.tenant_secret) != identity.tenant_id
        || !context_relay::valid_registration_proof(
            &identity.tenant_id,
            &identity.tenant_secret,
            identity.registration_nonce,
        )
    {
        return Err("relay identity file contains invalid values".to_string());
    }
    Ok(())
}

fn set_private_permissions(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn default_identity_file() -> String {
    format!(
        "{}/.codex/context-guardian/relay-identity.json",
        env::var("HOME").unwrap_or_else(|_| ".".to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_secure_blind_tunnel_urls() {
        let tenant = "a".repeat(32);
        assert_eq!(
            blind_tunnel_url("https://relay.example.com:5003", &tenant).unwrap(),
            format!("wss://relay.example.com:5003/v2/tunnel/{tenant}")
        );
        assert!(blind_tunnel_url("http://relay.example.com", &tenant).is_err());
    }

    #[test]
    fn restricts_blind_gateway_to_loopback() {
        assert!(validate_blind_gateway("127.0.0.1:8788").is_ok());
        assert!(validate_blind_gateway("[::1]:8788").is_ok());
        assert!(validate_blind_gateway("0.0.0.0:8788").is_err());
        assert!(validate_blind_gateway("192.0.2.1:8788").is_err());
    }
}
