use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_REQUEST_BYTES: usize = 16 * 1024;

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
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = next(&mut args, "--listen")?,
            "--cache-dir" => cache_dir = Some(PathBuf::from(next(&mut args, "--cache-dir")?)),
            "--signing-key-file" => {
                key_file = Some(PathBuf::from(next(&mut args, "--signing-key-file")?))
            }
            "--help" | "-h" => {
                return Err("Usage: context-image-gateway --cache-dir DIR --signing-key-file FILE [--listen [::]:8787]".to_string())
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
    let listener = TcpListener::bind(&listen).map_err(|error| error.to_string())?;
    println!("context-image-gateway listening on {listen}");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let cache_dir = cache_dir.clone();
                let signing_key = signing_key.clone();
                thread::spawn(move || {
                    if let Err(error) = handle(stream, &cache_dir, &signing_key) {
                        eprintln!("request failed: {error}");
                    }
                });
            }
            Err(error) => eprintln!("accept failed: {error}"),
        }
    }
    Ok(())
}

fn next(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn handle(mut stream: TcpStream, cache_dir: &Path, signing_key: &[u8]) -> io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut buffer = vec![0u8; MAX_REQUEST_BYTES];
    let read = stream.read(&mut buffer)?;
    if read == MAX_REQUEST_BYTES {
        return respond(&mut stream, 431, "text/plain", b"request too large", false);
    }
    let request = String::from_utf8_lossy(&buffer[..read]);
    let Some(line) = request.lines().next() else {
        return respond(&mut stream, 400, "text/plain", b"bad request", false);
    };
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" && method != "HEAD" {
        return respond(&mut stream, 405, "text/plain", b"method not allowed", false);
    }
    let Some((path, query)) = target.split_once('?') else {
        return respond(&mut stream, 403, "text/plain", b"signature required", false);
    };
    let Some(filename) = path.strip_prefix("/image/") else {
        return respond(&mut stream, 404, "text/plain", b"not found", false);
    };
    if !valid_filename(filename) {
        return respond(&mut stream, 400, "text/plain", b"invalid filename", false);
    }
    let expires = query_value(query, "expires").and_then(|value| value.parse::<u64>().ok());
    let signature = query_value(query, "sig");
    let (Some(expires), Some(signature)) = (expires, signature) else {
        return respond(&mut stream, 403, "text/plain", b"invalid signature", false);
    };
    if expires < unix_seconds() || !verify(signing_key, filename, expires, signature) {
        return respond(
            &mut stream,
            403,
            "text/plain",
            b"expired or invalid signature",
            false,
        );
    }
    let image = fs::read(cache_dir.join(filename))?;
    respond(
        &mut stream,
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
    stream: &mut TcpStream,
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
