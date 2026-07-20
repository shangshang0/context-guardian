use hmac::{Hmac, Mac};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use sha2::Sha256;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

fn main() {
    if let Err(error) = run() {
        eprintln!("context-image-gateway failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut listen = "[::]:8787".to_string();
    let mut cache_dir = None;
    let mut key_file = None;
    let mut tls_cert_file = None;
    let mut tls_key_file = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = next(&mut args, "--listen")?,
            "--cache-dir" => cache_dir = Some(PathBuf::from(next(&mut args, "--cache-dir")?)),
            "--signing-key-file" => {
                key_file = Some(PathBuf::from(next(&mut args, "--signing-key-file")?))
            }
            "--tls-cert-file" => {
                tls_cert_file = Some(PathBuf::from(next(&mut args, "--tls-cert-file")?))
            }
            "--tls-key-file" => {
                tls_key_file = Some(PathBuf::from(next(&mut args, "--tls-key-file")?))
            }
            "--help" | "-h" => {
                return Err("Usage: context-image-gateway --cache-dir DIR --signing-key-file FILE [--listen [::]:8787] [--tls-cert-file FILE --tls-key-file FILE]".to_string())
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let cache_dir = cache_dir.ok_or_else(|| "--cache-dir is required".to_string())?;
    let key_file = key_file.ok_or_else(|| "--signing-key-file is required".to_string())?;
    let signing_key = fs::read(key_file).map_err(|error| error.to_string())?;
    if signing_key.len() < 32 {
        return Err("signing key must contain at least 32 bytes".to_string());
    }
    let tls_config = match (tls_cert_file, tls_key_file) {
        (Some(cert_file), Some(key_file)) => Some(load_tls_config(&cert_file, &key_file)?),
        (None, None) => None,
        _ => {
            return Err("--tls-cert-file and --tls-key-file must be provided together".to_string())
        }
    };
    let listener = TcpListener::bind(&listen).map_err(|error| error.to_string())?;
    println!(
        "context-image-gateway listening on {listen} tls={}",
        tls_config.is_some()
    );
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let cache_dir = cache_dir.clone();
                let signing_key = signing_key.clone();
                let tls_config = tls_config.clone();
                thread::spawn(move || {
                    if let Err(error) =
                        handle_connection(stream, &cache_dir, &signing_key, tls_config)
                    {
                        eprintln!("request failed: {error}");
                    }
                });
            }
            Err(error) => eprintln!("accept failed: {error}"),
        }
    }
    Ok(())
}

fn load_tls_config(cert_file: &Path, key_file: &Path) -> Result<Arc<ServerConfig>, String> {
    let mut cert_reader = BufReader::new(File::open(cert_file).map_err(|error| error.to_string())?);
    let certificates = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if certificates.is_empty() {
        return Err("TLS certificate file contains no certificates".to_string());
    }
    let mut key_reader = BufReader::new(File::open(key_file).map_err(|error| error.to_string())?);
    let private_key = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "TLS key file contains no private key".to_string())?;
    let provider = rustls::crypto::ring::default_provider();
    let config = ServerConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|error| error.to_string())?;
    Ok(Arc::new(config))
}

fn next(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn handle_connection(
    stream: TcpStream,
    cache_dir: &Path,
    signing_key: &[u8],
    tls_config: Option<Arc<ServerConfig>>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(15)))?;
    if let Some(config) = tls_config {
        let connection = ServerConnection::new(config).map_err(io::Error::other)?;
        let mut tls = StreamOwned::new(connection, stream);
        handle(&mut tls, cache_dir, signing_key)
    } else {
        let mut stream = stream;
        handle(&mut stream, cache_dir, signing_key)
    }
}

fn handle(
    stream: &mut (impl Read + Write),
    cache_dir: &Path,
    signing_key: &[u8],
) -> io::Result<()> {
    let mut buffer = Vec::with_capacity(2048);
    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
        if buffer.len() >= MAX_REQUEST_BYTES {
            return respond(stream, 431, "text/plain", b"request too large", false);
        }
        let mut chunk = [0u8; 2048];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let request = String::from_utf8_lossy(&buffer);
    let Some(line) = request.lines().next() else {
        return respond(stream, 400, "text/plain", b"bad request", false);
    };
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" && method != "HEAD" {
        return respond(stream, 405, "text/plain", b"method not allowed", false);
    }
    let Some((path, query)) = target.split_once('?') else {
        return respond(stream, 403, "text/plain", b"signature required", false);
    };
    let Some(filename) = path.strip_prefix("/image/") else {
        return respond(stream, 404, "text/plain", b"not found", false);
    };
    if !valid_filename(filename) {
        return respond(stream, 400, "text/plain", b"invalid filename", false);
    }
    let expires = query_value(query, "expires").and_then(|value| value.parse::<u64>().ok());
    let signature = query_value(query, "sig");
    let (Some(expires), Some(signature)) = (expires, signature) else {
        return respond(stream, 403, "text/plain", b"invalid signature", false);
    };
    if expires < unix_seconds() || !verify(signing_key, filename, expires, signature) {
        return respond(
            stream,
            403,
            "text/plain",
            b"expired or invalid signature",
            false,
        );
    }
    let image_path = cache_dir.join(filename);
    if fs::metadata(&image_path)?.len() > MAX_IMAGE_BYTES {
        return respond(stream, 413, "text/plain", b"image too large", false);
    }
    let image = fs::read(image_path)?;
    respond(
        stream,
        200,
        content_type(filename),
        &image,
        method == "HEAD",
    )
}

fn valid_filename(filename: &str) -> bool {
    let Some((digest, extension)) = filename.split_once('.') else {
        return false;
    };
    digest.len() == 64
        && digest
            .chars()
            .all(|character| character.is_ascii_hexdigit())
        && matches!(extension, "png" | "jpg" | "webp" | "gif")
}

fn query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|part| {
        let (name, value) = part.split_once('=')?;
        (name == key).then_some(value)
    })
}

fn verify(key: &[u8], filename: &str, expires: u64, signature: &str) -> bool {
    let Ok(signature) = hex::decode(signature) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key) else {
        return false;
    };
    mac.update(format!("{filename}\n{expires}").as_bytes());
    mac.verify_slice(&signature).is_ok()
}

fn content_type(filename: &str) -> &'static str {
    if filename.ends_with(".jpg") {
        "image/jpeg"
    } else if filename.ends_with(".webp") {
        "image/webp"
    } else if filename.ends_with(".gif") {
        "image/gif"
    } else {
        "image/png"
    }
}

fn respond(
    stream: &mut impl Write,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: private, no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if !head_only {
        stream.write_all(body)?;
    }
    Ok(())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_cache_filenames() {
        assert!(valid_filename(&format!("{}.png", "a".repeat(64))));
        assert!(!valid_filename("../../secret.png"));
        assert!(!valid_filename(&format!("{}.svg", "a".repeat(64))));
    }

    #[test]
    fn verifies_signatures() {
        let key = b"01234567890123456789012345678901";
        let filename = format!("{}.png", "a".repeat(64));
        let expires = unix_seconds() + 60;
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(format!("{filename}\n{expires}").as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());
        assert!(verify(key, &filename, expires, &signature));
        assert!(!verify(key, &filename, expires + 1, &signature));
    }
}
