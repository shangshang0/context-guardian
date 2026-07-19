use base64::Engine;
use context_relay::{ClientIdentity, PollResponse, RegisterRequest, RelayResult, MAX_IMAGE_BYTES};
use reqwest::{Client, StatusCode};
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
            Err(error) => {
                eprintln!("relay poll failed: {error}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
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
) -> Result<(), String> {
    let base = relay_url.trim_end_matches('/');
    let response = relay_client
        .post(format!("{base}/v1/poll/{}", identity.tenant_id))
        .bearer_auth(&identity.tenant_secret)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status() == StatusCode::NO_CONTENT {
        return Ok(());
    }
    if !response.status().is_success() {
        return Err(format!("poll returned {}", response.status()));
    }
    let job: PollResponse = response.json().await.map_err(|error| error.to_string())?;
    let local = local_client
        .get(format!(
            "{}{}",
            local_gateway.trim_end_matches('/'),
            job.path_and_query
        ))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = local.status().as_u16();
    let content_type = local
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let body = local.bytes().await.map_err(|error| error.to_string())?;
    if body.len() > MAX_IMAGE_BYTES {
        return Err("local image exceeds size limit".to_string());
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
        .map_err(|error| error.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("result returned {}", response.status()))
    }
}

async fn load_or_create_identity(path: &Path) -> Result<ClientIdentity, String> {
    if let Ok(bytes) = tokio::fs::read(path).await {
        return serde_json::from_slice(&bytes).map_err(|error| error.to_string());
    }
    let identity = ClientIdentity::generate();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec(&identity).map_err(|error| error.to_string())?;
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| error.to_string())?;
    set_private_permissions(path)?;
    Ok(identity)
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
