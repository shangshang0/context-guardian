use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_SCHEMA_FIELDS: usize = 50_000;
const MAX_SCHEMA_DEPTH: usize = 64;

#[derive(Debug, Clone)]
pub struct CaptureOptions {
    pub interface: String,
    pub port: u16,
    pub duration: Duration,
    pub max_pcap_bytes: u64,
    pub max_reports: usize,
    pub report_dir: PathBuf,
    pub tcpdump: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureReport {
    pub preview_version: u32,
    pub capture_mode: String,
    pub interface: String,
    pub port: u16,
    pub started_at_unix_ms: u64,
    pub ended_at_unix_ms: u64,
    pub pcap_limit_reached: bool,
    pub transactions: Vec<TransactionReport>,
    pub privacy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionReport {
    pub request_at_unix_ms: u64,
    pub method: String,
    pub target_hash: String,
    pub content_encoding: String,
    pub encoded_body_bytes: usize,
    pub decoded_body_bytes: usize,
    pub body_hash: String,
    pub schema_hash: String,
    pub schema: Vec<SchemaField>,
    pub identifiers: BTreeMap<String, String>,
    pub response_status: Option<u16>,
    pub response_error_envelope: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaField {
    pub path: String,
    pub value_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_enum: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PassiveEvidence {
    pub attempted: bool,
    pub supports_lossless_repair: bool,
    pub status: String,
    pub failed_request_at_unix_ms: Option<u64>,
    pub baseline_request_at_unix_ms: Option<u64>,
    pub schema_differences: Vec<SchemaDifference>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchemaDifference {
    pub path: String,
    pub successful_type: String,
    pub failing_type: String,
    pub classification: String,
}

impl PassiveEvidence {
    pub fn not_requested() -> Self {
        Self {
            attempted: false,
            supports_lossless_repair: false,
            status: "not_requested".to_string(),
            failed_request_at_unix_ms: None,
            baseline_request_at_unix_ms: None,
            schema_differences: Vec::new(),
        }
    }

    pub fn as_json(&self) -> Value {
        json!({
            "attempted": self.attempted,
            "supports_lossless_repair": self.supports_lossless_repair,
            "status": self.status,
            "failed_request_at_unix_ms": self.failed_request_at_unix_ms,
            "baseline_request_at_unix_ms": self.baseline_request_at_unix_ms,
            "schema_differences": self.schema_differences,
        })
    }
}

struct EphemeralPcap {
    path: PathBuf,
}

impl Drop for EphemeralPcap {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn capture_once(options: &CaptureOptions) -> io::Result<PathBuf> {
    validate_options(options)?;
    secure_dir(&options.report_dir)?;
    let started_at_unix_ms = unix_ms();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let raw_path = options.report_dir.join(format!(
        ".passive-capture-{}-{stamp}.pcap",
        std::process::id()
    ));
    create_private_file(&raw_path)?;
    let raw = EphemeralPcap { path: raw_path };

    let mut child = Command::new(&options.tcpdump)
        .args(["-i", &options.interface, "-U", "-n", "-s", "0", "-w"])
        .arg(&raw.path)
        .args(["tcp", "port", &options.port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    let mut pcap_limit_reached = false;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() && started.elapsed() < Duration::from_millis(500) {
                return Err(io::Error::other(format!(
                    "tcpdump exited before capture began: {status}"
                )));
            }
            break;
        }
        let length = fs::metadata(&raw.path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if length >= options.max_pcap_bytes {
            pcap_limit_reached = true;
        }
        if pcap_limit_reached || started.elapsed() >= options.duration {
            stop_capture(&mut child)?;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    let bytes = fs::read(&raw.path)?;
    let transactions = analyze_pcap(&bytes, options.port, DEFAULT_MAX_BODY_BYTES)?;
    let ended_at_unix_ms = unix_ms();
    let report = CaptureReport {
        preview_version: 1,
        capture_mode: "passive_loopback_sidecar".to_string(),
        interface: options.interface.clone(),
        port: options.port,
        started_at_unix_ms,
        ended_at_unix_ms,
        pcap_limit_reached,
        transactions,
        privacy: "raw PCAP deleted after processing; authorization and header values, request bodies, and message scalar values are never written to reports".to_string(),
    };
    let report_path = options
        .report_dir
        .join(format!("passive-capture-{started_at_unix_ms}-{stamp}.json"));
    write_private_json(&report_path, &report)?;
    prune_old_reports(&options.report_dir, options.max_reports)?;
    drop(raw);
    Ok(report_path)
}

fn stop_capture(child: &mut std::process::Child) -> io::Result<()> {
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if child.try_wait()?.is_some() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
    child.kill()?;
    let _ = child.wait();
    Ok(())
}

fn validate_options(options: &CaptureOptions) -> io::Result<()> {
    if options.port == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "port must be non-zero",
        ));
    }
    if options.interface.is_empty()
        || options.interface.len() > 64
        || !options.interface.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "interface contains unsafe characters",
        ));
    }
    if !(Duration::from_secs(1)..=Duration::from_secs(900)).contains(&options.duration) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capture duration must be between 1 and 900 seconds",
        ));
    }
    if !(64 * 1024..=256 * 1024 * 1024).contains(&options.max_pcap_bytes) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PCAP limit must be between 64 KiB and 256 MiB",
        ));
    }
    if !(2..=10_000).contains(&options.max_reports) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "report retention must be between 2 and 10000 files",
        ));
    }
    if !options.tcpdump.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "tcpdump executable was not found",
        ));
    }
    Ok(())
}

fn prune_old_reports(report_dir: &Path, max_reports: usize) -> io::Result<()> {
    let mut reports = fs::read_dir(report_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("passive-capture-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    reports.sort();
    let remove_count = reports.len().saturating_sub(max_reports);
    for path in reports.into_iter().take(remove_count) {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn secure_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn create_private_file(path: &Path) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?;
    Ok(())
}

fn write_private_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    serde_json::to_writer_pretty(&mut file, value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct Endpoint {
    address: Vec<u8>,
    port: u16,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct FlowKey {
    client: Endpoint,
    server: Endpoint,
}

#[derive(Debug, Clone)]
struct Segment {
    sequence: u32,
    timestamp_ms: u64,
    bytes: Vec<u8>,
}

#[derive(Debug, Default)]
struct Flow {
    requests: Vec<Segment>,
    responses: Vec<Segment>,
}

#[derive(Debug)]
struct HttpMessage {
    timestamp_ms: u64,
    start_line: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    encoded_body_bytes: usize,
}

#[derive(Debug)]
struct ByteStream {
    bytes: Vec<u8>,
    marks: Vec<(usize, u64)>,
}

impl ByteStream {
    fn timestamp_for(&self, offset: usize) -> u64 {
        self.marks
            .iter()
            .rev()
            .find_map(|(position, timestamp)| (*position <= offset).then_some(*timestamp))
            .or_else(|| self.marks.first().map(|(_, timestamp)| *timestamp))
            .unwrap_or_default()
    }
}

pub fn analyze_pcap(
    bytes: &[u8],
    server_port: u16,
    max_body_bytes: usize,
) -> io::Result<Vec<TransactionReport>> {
    let (packets, link_type) = parse_pcap(bytes)?;
    let mut flows: HashMap<FlowKey, Flow> = HashMap::new();
    for packet in packets {
        let Some(parsed) = parse_tcp_packet(packet.bytes, link_type) else {
            continue;
        };
        let (key, request_direction) = if parsed.destination.port == server_port {
            (
                FlowKey {
                    client: parsed.source,
                    server: parsed.destination,
                },
                true,
            )
        } else if parsed.source.port == server_port {
            (
                FlowKey {
                    client: parsed.destination,
                    server: parsed.source,
                },
                false,
            )
        } else {
            continue;
        };
        if parsed.payload.is_empty() {
            continue;
        }
        let segment = Segment {
            sequence: parsed.sequence,
            timestamp_ms: packet.timestamp_ms,
            bytes: parsed.payload.to_vec(),
        };
        let flow = flows.entry(key).or_default();
        if request_direction {
            flow.requests.push(segment);
        } else {
            flow.responses.push(segment);
        }
    }

    let mut transactions = Vec::new();
    for flow in flows.into_values() {
        let request_stream = reassemble(flow.requests);
        let response_stream = reassemble(flow.responses);
        let requests = parse_http_messages(&request_stream, false, max_body_bytes)?;
        let responses = parse_http_messages(&response_stream, true, max_body_bytes)?;
        for (index, request) in requests.iter().enumerate() {
            if let Some(report) = request_report(request, responses.get(index))? {
                transactions.push(report);
            }
        }
    }
    transactions.sort_by_key(|transaction| transaction.request_at_unix_ms);
    Ok(transactions)
}

struct PcapPacket<'a> {
    timestamp_ms: u64,
    bytes: &'a [u8],
}

fn parse_pcap(bytes: &[u8]) -> io::Result<(Vec<PcapPacket<'_>>, u32)> {
    if bytes.len() < 24 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "PCAP header is incomplete",
        ));
    }
    let (little_endian, nanos) = match &bytes[..4] {
        [0xd4, 0xc3, 0xb2, 0xa1] => (true, false),
        [0xa1, 0xb2, 0xc3, 0xd4] => (false, false),
        [0x4d, 0x3c, 0xb2, 0xa1] => (true, true),
        [0xa1, 0xb2, 0x3c, 0x4d] => (false, true),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported PCAP magic",
            ))
        }
    };
    let read_u32 = |slice: &[u8]| {
        let raw: [u8; 4] = slice.try_into().expect("four-byte PCAP field");
        if little_endian {
            u32::from_le_bytes(raw)
        } else {
            u32::from_be_bytes(raw)
        }
    };
    let link_type = read_u32(&bytes[20..24]);
    let mut packets = Vec::new();
    let mut offset = 24;
    while offset < bytes.len() {
        if bytes.len() - offset < 16 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "PCAP packet header is incomplete",
            ));
        }
        let seconds = read_u32(&bytes[offset..offset + 4]) as u64;
        let fraction = read_u32(&bytes[offset + 4..offset + 8]) as u64;
        let included = read_u32(&bytes[offset + 8..offset + 12]) as usize;
        offset += 16;
        if included > bytes.len() - offset {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "PCAP packet data is incomplete",
            ));
        }
        let fraction_ms = if nanos {
            fraction / 1_000_000
        } else {
            fraction / 1_000
        };
        packets.push(PcapPacket {
            timestamp_ms: seconds.saturating_mul(1_000).saturating_add(fraction_ms),
            bytes: &bytes[offset..offset + included],
        });
        offset += included;
    }
    Ok((packets, link_type))
}

struct ParsedTcp<'a> {
    source: Endpoint,
    destination: Endpoint,
    sequence: u32,
    payload: &'a [u8],
}

fn parse_tcp_packet(packet: &[u8], link_type: u32) -> Option<ParsedTcp<'_>> {
    let (network, protocol_hint) = match link_type {
        0 if packet.len() >= 4 => {
            let family = u32::from_ne_bytes(packet[..4].try_into().ok()?);
            (&packet[4..], Some(family))
        }
        1 if packet.len() >= 14 => {
            let ether_type = u16::from_be_bytes(packet[12..14].try_into().ok()?);
            (&packet[14..], Some(ether_type as u32))
        }
        113 if packet.len() >= 16 => {
            let protocol = u16::from_be_bytes(packet[14..16].try_into().ok()?);
            (&packet[16..], Some(protocol as u32))
        }
        276 if packet.len() >= 20 => {
            let protocol = u16::from_be_bytes(packet[..2].try_into().ok()?);
            (&packet[20..], Some(protocol as u32))
        }
        _ => return None,
    };
    let version = network.first().map(|byte| byte >> 4)?;
    match version {
        4 if matches!(protocol_hint, Some(2 | 0x0800) | None) => parse_ipv4_tcp(network),
        6 if matches!(protocol_hint, Some(30 | 24 | 28 | 0x86dd) | None) => parse_ipv6_tcp(network),
        _ => None,
    }
}

fn parse_ipv4_tcp(packet: &[u8]) -> Option<ParsedTcp<'_>> {
    if packet.len() < 20 || packet[9] != 6 {
        return None;
    }
    let header_length = usize::from(packet[0] & 0x0f) * 4;
    if header_length < 20 || packet.len() < header_length + 20 {
        return None;
    }
    parse_tcp(
        &packet[header_length..],
        packet[12..16].to_vec(),
        packet[16..20].to_vec(),
    )
}

fn parse_ipv6_tcp(packet: &[u8]) -> Option<ParsedTcp<'_>> {
    if packet.len() < 60 || packet[6] != 6 {
        return None;
    }
    parse_tcp(
        &packet[40..],
        packet[8..24].to_vec(),
        packet[24..40].to_vec(),
    )
}

fn parse_tcp(
    packet: &[u8],
    source_address: Vec<u8>,
    destination_address: Vec<u8>,
) -> Option<ParsedTcp<'_>> {
    if packet.len() < 20 {
        return None;
    }
    let source_port = u16::from_be_bytes(packet[..2].try_into().ok()?);
    let destination_port = u16::from_be_bytes(packet[2..4].try_into().ok()?);
    let sequence = u32::from_be_bytes(packet[4..8].try_into().ok()?);
    let header_length = usize::from(packet[12] >> 4) * 4;
    if header_length < 20 || header_length > packet.len() {
        return None;
    }
    Some(ParsedTcp {
        source: Endpoint {
            address: source_address,
            port: source_port,
        },
        destination: Endpoint {
            address: destination_address,
            port: destination_port,
        },
        sequence,
        payload: &packet[header_length..],
    })
}

fn reassemble(mut segments: Vec<Segment>) -> ByteStream {
    segments.sort_by_key(|segment| segment.sequence);
    let Some(first) = segments.first() else {
        return ByteStream {
            bytes: Vec::new(),
            marks: Vec::new(),
        };
    };
    let base = first.sequence;
    let mut stream = ByteStream {
        bytes: Vec::new(),
        marks: Vec::new(),
    };
    for segment in segments {
        let start = segment.sequence.wrapping_sub(base) as usize;
        if start > stream.bytes.len() {
            continue;
        }
        let overlap = stream.bytes.len().saturating_sub(start);
        if overlap >= segment.bytes.len() {
            continue;
        }
        stream
            .marks
            .push((stream.bytes.len(), segment.timestamp_ms));
        stream.bytes.extend_from_slice(&segment.bytes[overlap..]);
    }
    stream
}

fn parse_http_messages(
    stream: &ByteStream,
    response: bool,
    max_body_bytes: usize,
) -> io::Result<Vec<HttpMessage>> {
    let mut messages = Vec::new();
    let mut offset = 0;
    while offset < stream.bytes.len() {
        let Some(start) = find_http_start(&stream.bytes, offset, response) else {
            break;
        };
        let Some(header_end) = find_bytes(&stream.bytes, b"\r\n\r\n", start) else {
            break;
        };
        let header_bytes = &stream.bytes[start..header_end];
        let header_text = std::str::from_utf8(header_bytes).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "HTTP headers are not UTF-8")
        })?;
        let mut lines = header_text.split("\r\n");
        let start_line = lines.next().unwrap_or_default().to_string();
        let mut headers = HashMap::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                let lower = name.trim().to_ascii_lowercase();
                if matches!(
                    lower.as_str(),
                    "content-length" | "transfer-encoding" | "content-encoding" | "content-type"
                ) {
                    headers.insert(lower, value.trim().to_ascii_lowercase());
                }
            }
        }
        let body_start = header_end + 4;
        let (encoded, consumed) = if headers
            .get("transfer-encoding")
            .is_some_and(|value| value.split(',').any(|part| part.trim() == "chunked"))
        {
            let Some((decoded, consumed)) =
                decode_chunked(&stream.bytes[body_start..], max_body_bytes)?
            else {
                break;
            };
            (decoded, consumed)
        } else {
            let length = headers
                .get("content-length")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            if length > max_body_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP body exceeds limit",
                ));
            }
            if stream.bytes.len() - body_start < length {
                break;
            }
            (
                stream.bytes[body_start..body_start + length].to_vec(),
                length,
            )
        };
        let encoded_body_bytes = encoded.len();
        let body = decode_content(&encoded, headers.get("content-encoding"), max_body_bytes)?;
        messages.push(HttpMessage {
            timestamp_ms: stream.timestamp_for(start),
            start_line,
            headers,
            body,
            encoded_body_bytes,
        });
        offset = body_start + consumed;
    }
    Ok(messages)
}

fn find_http_start(bytes: &[u8], offset: usize, response: bool) -> Option<usize> {
    let patterns: &[&[u8]] = if response {
        &[b"HTTP/1.1 ", b"HTTP/1.0 "]
    } else {
        &[b"POST ", b"PUT ", b"PATCH ", b"GET ", b"DELETE "]
    };
    patterns
        .iter()
        .filter_map(|pattern| find_bytes(bytes, pattern, offset))
        .min()
}

fn find_bytes(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    haystack
        .get(start..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| start + position)
}

fn decode_chunked(bytes: &[u8], max_body_bytes: usize) -> io::Result<Option<(Vec<u8>, usize)>> {
    let mut decoded = Vec::new();
    let mut offset = 0;
    loop {
        let Some(line_end) = find_bytes(bytes, b"\r\n", offset) else {
            return Ok(None);
        };
        let size_text = std::str::from_utf8(&bytes[offset..line_end])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk size is not UTF-8"))?;
        let size =
            usize::from_str_radix(size_text.split(';').next().unwrap_or_default().trim(), 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid chunk size"))?;
        offset = line_end + 2;
        if size == 0 {
            let Some(trailer_end) = find_bytes(bytes, b"\r\n", offset) else {
                return Ok(None);
            };
            return Ok(Some((decoded, trailer_end + 2)));
        }
        if size > max_body_bytes.saturating_sub(decoded.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "decoded chunked body exceeds limit",
            ));
        }
        if bytes.len() < offset + size + 2 {
            return Ok(None);
        }
        decoded.extend_from_slice(&bytes[offset..offset + size]);
        if &bytes[offset + size..offset + size + 2] != b"\r\n" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chunk is missing terminator",
            ));
        }
        offset += size + 2;
    }
}

fn decode_content(
    bytes: &[u8],
    encoding: Option<&String>,
    max_body_bytes: usize,
) -> io::Result<Vec<u8>> {
    if !encoding.is_some_and(|value| value.split(',').any(|part| part.trim() == "gzip")) {
        return Ok(bytes.to_vec());
    }
    let mut decoder = GzDecoder::new(bytes);
    let mut output = Vec::new();
    decoder
        .by_ref()
        .take(max_body_bytes as u64 + 1)
        .read_to_end(&mut output)?;
    if output.len() > max_body_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "decompressed body exceeds limit",
        ));
    }
    Ok(output)
}

fn request_report(
    request: &HttpMessage,
    response: Option<&HttpMessage>,
) -> io::Result<Option<TransactionReport>> {
    let mut start_parts = request.start_line.split_whitespace();
    let method = start_parts.next().unwrap_or_default();
    let target = start_parts.next().unwrap_or_default();
    if method.is_empty() || target.is_empty() {
        return Ok(None);
    }
    let value: Value = match serde_json::from_slice(&request.body) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let mut schema = Vec::new();
    collect_schema(&value, "$", None, 0, &mut schema)?;
    let schema_serialized = serde_json::to_vec(&schema)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut identifiers = BTreeMap::new();
    collect_identifiers(&value, "$", &mut identifiers, 0);
    let (response_status, response_error_envelope) = response
        .map(|message| {
            (
                parse_response_status(&message.start_line),
                response_has_error(message),
            )
        })
        .unwrap_or((None, false));
    Ok(Some(TransactionReport {
        request_at_unix_ms: request.timestamp_ms,
        method: method.to_string(),
        target_hash: sha256_hex(target.as_bytes()),
        content_encoding: request.headers.get("content-encoding").map_or_else(
            || "identity".to_string(),
            |value| safe_content_encoding(value),
        ),
        encoded_body_bytes: request.encoded_body_bytes,
        decoded_body_bytes: request.body.len(),
        body_hash: sha256_hex(&request.body),
        schema_hash: sha256_hex(&schema_serialized),
        schema,
        identifiers,
        response_status,
        response_error_envelope,
    }))
}

fn collect_schema(
    value: &Value,
    path: &str,
    key: Option<&str>,
    depth: usize,
    output: &mut Vec<SchemaField>,
) -> io::Result<()> {
    if depth > MAX_SCHEMA_DEPTH || output.len() >= MAX_SCHEMA_FIELDS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "JSON schema exceeds safety limit",
        ));
    }
    let value_type = value_kind(value).to_string();
    let safe_enum = match (key, value.as_str()) {
        (Some("role"), Some(text)) => Some(safe_enum("role", text)),
        (Some("type"), Some(text)) => Some(safe_enum("type", text)),
        _ => None,
    };
    output.push(SchemaField {
        path: path.to_string(),
        value_type,
        safe_enum,
    });
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_schema(item, &format!("{path}[{index}]"), None, depth + 1, output)?;
            }
        }
        Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(name, _)| *name);
            for (name, item) in entries {
                collect_schema(
                    item,
                    &format!("{path}.{}", escape_path(name)),
                    Some(name),
                    depth + 1,
                    output,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_identifiers(
    value: &Value,
    path: &str,
    output: &mut BTreeMap<String, String>,
    depth: usize,
) {
    if depth > MAX_SCHEMA_DEPTH || output.len() >= 64 {
        return;
    }
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_identifiers(item, &format!("{path}[{index}]"), output, depth + 1);
            }
        }
        Value::Object(map) => {
            for (name, item) in map {
                let item_path = format!("{path}.{}", escape_path(name));
                if is_identifier_key(name) {
                    if let Some(bytes) = scalar_bytes(item) {
                        output.insert(item_path.clone(), sha256_hex(&bytes));
                    }
                }
                collect_identifiers(item, &item_path, output, depth + 1);
            }
        }
        _ => {}
    }
}

fn is_identifier_key(key: &str) -> bool {
    matches!(
        key,
        "request_id" | "turn_id" | "thread_id" | "conversation_id" | "prompt_cache_key" | "call_id"
    )
}

fn scalar_bytes(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::String(text) => Some(text.as_bytes().to_vec()),
        Value::Number(number) => Some(number.to_string().into_bytes()),
        _ => None,
    }
}

fn safe_enum(kind: &str, value: &str) -> String {
    let allowed = match kind {
        "role" => matches!(
            value,
            "user" | "assistant" | "system" | "developer" | "tool"
        ),
        "type" => matches!(
            value,
            "message"
                | "input_text"
                | "output_text"
                | "input_image"
                | "function_call"
                | "function_call_output"
                | "computer_call"
                | "computer_call_output"
                | "reasoning"
                | "error"
                | "response.completed"
                | "response.failed"
        ),
        _ => false,
    };
    if allowed {
        value.to_string()
    } else {
        format!("other:{}", &sha256_hex(value.as_bytes())[..16])
    }
}

fn escape_path(name: &str) -> String {
    if is_protocol_field(name) {
        name.to_string()
    } else {
        format!("[key:{}]", &sha256_hex(name.as_bytes())[..16])
    }
}

fn is_protocol_field(name: &str) -> bool {
    matches!(
        name,
        "id" | "model"
            | "input"
            | "output"
            | "role"
            | "content"
            | "type"
            | "text"
            | "arguments"
            | "name"
            | "call_id"
            | "tool_call_id"
            | "tools"
            | "instructions"
            | "replacement_history"
            | "previous_response_id"
            | "stream"
            | "store"
            | "metadata"
            | "request_id"
            | "turn_id"
            | "thread_id"
            | "conversation_id"
            | "prompt_cache_key"
            | "status"
            | "error"
            | "message"
            | "reasoning"
            | "summary"
            | "include"
            | "temperature"
            | "top_p"
            | "max_output_tokens"
            | "parallel_tool_calls"
            | "tool_choice"
    )
}

fn safe_content_encoding(value: &str) -> String {
    let normalized = value.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "identity" | "gzip") {
        normalized
    } else {
        format!("other:{}", &sha256_hex(normalized.as_bytes())[..16])
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn parse_response_status(line: &str) -> Option<u16> {
    line.split_whitespace().nth(1)?.parse().ok()
}

fn response_has_error(response: &HttpMessage) -> bool {
    if parse_response_status(&response.start_line).is_some_and(|status| status >= 400) {
        return true;
    }
    if let Ok(value) = serde_json::from_slice::<Value>(&response.body) {
        return json_has_error(&value);
    }
    std::str::from_utf8(&response.body)
        .ok()
        .is_some_and(|text| {
            text.lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
                .any(|value| json_has_error(&value))
        })
}

fn json_has_error(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("error").is_some_and(|error| !error.is_null())
                || map
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| matches!(kind, "error" | "response.failed"))
                || map.values().any(json_has_error)
        }
        Value::Array(items) => items.iter().any(json_has_error),
        _ => false,
    }
}

pub fn correlate_reports(
    report_dir: &Path,
    error_unix_ms: Option<u64>,
    window: Duration,
) -> PassiveEvidence {
    match correlate_reports_inner(report_dir, error_unix_ms, window) {
        Ok(evidence) => evidence,
        Err(error) => PassiveEvidence {
            attempted: true,
            supports_lossless_repair: false,
            status: format!("capture_report_error:{:?}", error.kind()),
            failed_request_at_unix_ms: None,
            baseline_request_at_unix_ms: None,
            schema_differences: Vec::new(),
        },
    }
}

fn correlate_reports_inner(
    report_dir: &Path,
    error_unix_ms: Option<u64>,
    window: Duration,
) -> io::Result<PassiveEvidence> {
    let mut paths = fs::read_dir(report_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("passive-capture-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    let mut transactions = Vec::new();
    for path in paths.into_iter().rev().take(100) {
        let file = OpenOptions::new().read(true).open(path)?;
        let report: CaptureReport = serde_json::from_reader(file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        transactions.extend(report.transactions);
    }
    transactions.sort_by_key(|transaction| transaction.request_at_unix_ms);
    let reference = error_unix_ms.unwrap_or_else(unix_ms);
    let window_ms = window.as_millis().min(u128::from(u64::MAX)) as u64;
    let failed = transactions
        .iter()
        .filter(|transaction| is_failed(transaction))
        .filter(|transaction| transaction.request_at_unix_ms.abs_diff(reference) <= window_ms)
        .min_by_key(|transaction| transaction.request_at_unix_ms.abs_diff(reference));
    let Some(failed) = failed else {
        return Ok(PassiveEvidence {
            attempted: true,
            supports_lossless_repair: false,
            status: "no_correlated_failed_request".to_string(),
            failed_request_at_unix_ms: None,
            baseline_request_at_unix_ms: None,
            schema_differences: Vec::new(),
        });
    };
    let baseline = transactions
        .iter()
        .filter(|transaction| is_success(transaction))
        .filter(|transaction| transaction.request_at_unix_ms < failed.request_at_unix_ms)
        .filter(|transaction| transaction.target_hash == failed.target_hash)
        .filter(|transaction| identifiers_compatible(transaction, failed))
        .max_by_key(|transaction| transaction.request_at_unix_ms);
    let Some(baseline) = baseline else {
        return Ok(PassiveEvidence {
            attempted: true,
            supports_lossless_repair: false,
            status: "no_prior_successful_baseline".to_string(),
            failed_request_at_unix_ms: Some(failed.request_at_unix_ms),
            baseline_request_at_unix_ms: None,
            schema_differences: Vec::new(),
        });
    };
    let differences = schema_differences(&baseline.schema, &failed.schema);
    let supports = !differences.is_empty()
        && differences
            .iter()
            .all(|difference| difference.classification == "lossless_known_transform");
    Ok(PassiveEvidence {
        attempted: true,
        supports_lossless_repair: supports,
        status: if supports {
            "lossless_schema_delta".to_string()
        } else if differences.is_empty() {
            "no_schema_delta".to_string()
        } else {
            "ambiguous_schema_delta".to_string()
        },
        failed_request_at_unix_ms: Some(failed.request_at_unix_ms),
        baseline_request_at_unix_ms: Some(baseline.request_at_unix_ms),
        schema_differences: differences,
    })
}

fn is_failed(transaction: &TransactionReport) -> bool {
    transaction
        .response_status
        .is_some_and(|status| status >= 400)
        || transaction.response_error_envelope
}

fn is_success(transaction: &TransactionReport) -> bool {
    transaction
        .response_status
        .is_some_and(|status| (200..400).contains(&status))
        && !transaction.response_error_envelope
}

fn identifiers_compatible(left: &TransactionReport, right: &TransactionReport) -> bool {
    let left_values = left.identifiers.values().collect::<Vec<_>>();
    let right_values = right.identifiers.values().collect::<Vec<_>>();
    left_values.is_empty()
        || right_values.is_empty()
        || left_values.iter().any(|value| right_values.contains(value))
}

fn schema_differences(success: &[SchemaField], failure: &[SchemaField]) -> Vec<SchemaDifference> {
    let success = success
        .iter()
        .map(|field| (field.path.as_str(), field))
        .collect::<HashMap<_, _>>();
    let failure = failure
        .iter()
        .map(|field| (field.path.as_str(), field))
        .collect::<HashMap<_, _>>();
    let mut paths = success
        .keys()
        .chain(failure.keys())
        .copied()
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();
    let known_transform_roots = paths
        .iter()
        .filter_map(|path| {
            let good = success.get(path);
            let bad = failure.get(path);
            (classify_difference(path, good, bad) == "lossless_known_transform")
                .then_some((*path).to_string())
        })
        .collect::<Vec<_>>();
    let mut differences = Vec::new();
    for path in paths {
        let good = success.get(path);
        let bad = failure.get(path);
        let same = matches!((good, bad), (Some(good), Some(bad)) if good.value_type == bad.value_type && good.safe_enum == bad.safe_enum);
        if same {
            continue;
        }
        if known_transform_roots
            .iter()
            .any(|root| path != root && is_descendant_path(path, root))
            || ((good.is_none() || bad.is_none())
                && is_array_sequence_variation(path, &success, &failure))
        {
            continue;
        }
        let good_type = good
            .map(|field| field_signature(field))
            .unwrap_or_else(|| "missing".to_string());
        let bad_type = bad
            .map(|field| field_signature(field))
            .unwrap_or_else(|| "missing".to_string());
        let classification = classify_difference(path, good, bad).to_string();
        differences.push(SchemaDifference {
            path: path.to_string(),
            successful_type: good_type,
            failing_type: bad_type,
            classification,
        });
    }
    differences
}

fn is_descendant_path(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('['))
}

fn is_array_sequence_variation(
    path: &str,
    success: &HashMap<&str, &SchemaField>,
    failure: &HashMap<&str, &SchemaField>,
) -> bool {
    let Some(index_start) = path.find('[') else {
        return false;
    };
    let array_path = &path[..index_start];
    success
        .get(array_path)
        .is_some_and(|field| field.value_type == "array")
        && failure
            .get(array_path)
            .is_some_and(|field| field.value_type == "array")
}

fn field_signature(field: &SchemaField) -> String {
    field.safe_enum.as_ref().map_or_else(
        || field.value_type.clone(),
        |safe_enum| format!("{}:{safe_enum}", field.value_type),
    )
}

fn classify_difference(
    path: &str,
    good: Option<&&SchemaField>,
    bad: Option<&&SchemaField>,
) -> &'static str {
    let Some(good) = good else { return "ambiguous" };
    let Some(bad) = bad else { return "ambiguous" };
    let known_type_change = ((path.ends_with(".replacement_history")
        || path.ends_with(".content"))
        && good.value_type == "array"
        && bad.value_type == "string")
        || (path.ends_with(".arguments")
            && good.value_type == "string"
            && matches!(bad.value_type.as_str(), "object" | "array"))
        || (path.contains(".content[")
            && good.value_type == "object"
            && bad.value_type == "string")
        || (path.ends_with(".output")
            && matches!(good.value_type.as_str(), "string" | "array")
            && matches!(bad.value_type.as_str(), "object" | "array"));
    let known_enum_change = path.ends_with(".type")
        && good.value_type == "string"
        && bad.value_type == "string"
        && matches!(
            good.safe_enum.as_deref(),
            Some("input_text" | "output_text")
        )
        && matches!(bad.safe_enum.as_deref(), Some("input_text" | "output_text"));
    if known_type_change || known_enum_change {
        "lossless_known_transform"
    } else {
        "ambiguous"
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};

    #[test]
    fn parses_synthetic_pcap_content_length_and_redacts_scalars() {
        let body = br#"{"model":"secret-model","input":[{"role":"user","content":[{"type":"input_text","text":"never persist me"}]}],"thread_id":"thread-secret","filename-secret":"value"}"#;
        let request = http_request(body, &[]);
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec();
        let pcap = synthetic_pcap(&request, &response, 15721, 1_700_000_000);
        let reports = analyze_pcap(&pcap, 15721, 1024 * 1024).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].response_status, Some(200));
        assert!(reports[0].schema.iter().any(|field| {
            field.path == "$.input[0].content[0].text" && field.value_type == "string"
        }));
        let serialized = serde_json::to_string(&reports).unwrap();
        assert!(!serialized.contains("never persist me"));
        assert!(!serialized.contains("thread-secret"));
        assert!(!serialized.contains("secret-model"));
        assert!(!serialized.contains("filename-secret"));
    }

    #[test]
    fn parses_synthetic_pcap_chunked_gzip_request() {
        let body = br#"{"input":[{"role":"assistant","content":[{"type":"output_text","text":"compressed secret"}]}]}"#;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(body).unwrap();
        let gzip = encoder.finish().unwrap();
        let chunks = format!("{:x}\r\n", gzip.len()).into_bytes();
        let mut encoded = chunks;
        encoded.extend_from_slice(&gzip);
        encoded.extend_from_slice(b"\r\n0\r\n\r\n");
        let mut request = b"POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer forbidden\r\nTransfer-Encoding: chunked\r\nContent-Encoding: gzip\r\n\r\n".to_vec();
        request.extend_from_slice(&encoded);
        let response_body = br#"{"error":{"type":"unknown_error","message":"also secret"}}"#;
        let response = format!(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\n\r\n",
            response_body.len()
        )
        .into_bytes();
        let mut response_full = response;
        response_full.extend_from_slice(response_body);
        let pcap = synthetic_pcap_split(&request, &response_full, 15721, 1_700_000_100, 37);
        let reports = analyze_pcap(&pcap, 15721, 1024 * 1024).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].content_encoding, "gzip");
        assert_eq!(reports[0].decoded_body_bytes, body.len());
        assert!(reports[0].response_error_envelope);
        let serialized = serde_json::to_string(&reports).unwrap();
        assert!(!serialized.contains("compressed secret"));
        assert!(!serialized.contains("forbidden"));
        assert!(!serialized.contains("also secret"));
    }

    #[test]
    fn classifies_only_known_lossless_schema_changes() {
        let good = vec![SchemaField {
            path: "$.input[0].content".to_string(),
            value_type: "array".to_string(),
            safe_enum: None,
        }];
        let bad = vec![SchemaField {
            path: "$.input[0].content".to_string(),
            value_type: "string".to_string(),
            safe_enum: None,
        }];
        let differences = schema_differences(&good, &bad);
        assert_eq!(differences[0].classification, "lossless_known_transform");
    }

    #[test]
    fn correlates_failed_capture_with_successful_baseline() {
        let stamp = unix_ms();
        let directory = std::env::temp_dir().join(format!(
            "context-guardian-capture-correlation-{}-{stamp}",
            std::process::id()
        ));
        secure_dir(&directory).unwrap();
        let common = |timestamp, status, schema| TransactionReport {
            request_at_unix_ms: timestamp,
            method: "POST".to_string(),
            target_hash: "target-hash".to_string(),
            content_encoding: "identity".to_string(),
            encoded_body_bytes: 100,
            decoded_body_bytes: 100,
            body_hash: format!("body-{timestamp}"),
            schema_hash: format!("schema-{timestamp}"),
            schema,
            identifiers: BTreeMap::from([("$.thread_id".to_string(), "thread-hash".to_string())]),
            response_status: Some(status),
            response_error_envelope: status >= 400,
        };
        let report = CaptureReport {
            preview_version: 1,
            capture_mode: "passive_loopback_sidecar".to_string(),
            interface: "lo0".to_string(),
            port: 15721,
            started_at_unix_ms: stamp - 2_000,
            ended_at_unix_ms: stamp + 1_000,
            pcap_limit_reached: false,
            transactions: vec![
                common(
                    stamp - 1_000,
                    200,
                    vec![
                        SchemaField {
                            path: "$.input[0].content".to_string(),
                            value_type: "array".to_string(),
                            safe_enum: None,
                        },
                        SchemaField {
                            path: "$.input[0].content[0]".to_string(),
                            value_type: "object".to_string(),
                            safe_enum: None,
                        },
                    ],
                ),
                common(
                    stamp,
                    500,
                    vec![SchemaField {
                        path: "$.input[0].content".to_string(),
                        value_type: "string".to_string(),
                        safe_enum: None,
                    }],
                ),
            ],
            privacy: "schema only".to_string(),
        };
        write_private_json(&directory.join("passive-capture-test.json"), &report).unwrap();
        let evidence = correlate_reports(&directory, Some(stamp), Duration::from_secs(30));
        assert!(evidence.supports_lossless_repair);
        assert_eq!(evidence.status, "lossless_schema_delta");
        fs::remove_dir_all(directory).unwrap();
    }

    fn http_request(body: &[u8], extra_headers: &[u8]) -> Vec<u8> {
        let mut request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer forbidden\r\nContent-Length: {}\r\n",
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(extra_headers);
        request.extend_from_slice(b"\r\n");
        request.extend_from_slice(body);
        request
    }

    fn synthetic_pcap(request: &[u8], response: &[u8], port: u16, seconds: u32) -> Vec<u8> {
        synthetic_pcap_split(request, response, port, seconds, usize::MAX)
    }

    fn synthetic_pcap_split(
        request: &[u8],
        response: &[u8],
        port: u16,
        seconds: u32,
        split: usize,
    ) -> Vec<u8> {
        let mut pcap = Vec::new();
        pcap.extend_from_slice(&0xa1b2c3d4u32.to_le_bytes());
        pcap.extend_from_slice(&2u16.to_le_bytes());
        pcap.extend_from_slice(&4u16.to_le_bytes());
        pcap.extend_from_slice(&0i32.to_le_bytes());
        pcap.extend_from_slice(&0u32.to_le_bytes());
        pcap.extend_from_slice(&65535u32.to_le_bytes());
        pcap.extend_from_slice(&0u32.to_le_bytes());
        let request_chunks = request.chunks(split.max(1));
        let mut sequence = 1000u32;
        for (index, chunk) in request_chunks.enumerate() {
            let packet = null_ipv4_tcp(45123, port, sequence, chunk);
            push_pcap_packet(&mut pcap, seconds, index as u32 * 1000, &packet);
            sequence = sequence.wrapping_add(chunk.len() as u32);
        }
        let response_chunks = response.chunks(split.max(1));
        let mut sequence = 9000u32;
        for (index, chunk) in response_chunks.enumerate() {
            let packet = null_ipv4_tcp(port, 45123, sequence, chunk);
            push_pcap_packet(&mut pcap, seconds + 1, index as u32 * 1000, &packet);
            sequence = sequence.wrapping_add(chunk.len() as u32);
        }
        pcap
    }

    fn push_pcap_packet(pcap: &mut Vec<u8>, seconds: u32, micros: u32, packet: &[u8]) {
        pcap.extend_from_slice(&seconds.to_le_bytes());
        pcap.extend_from_slice(&micros.to_le_bytes());
        pcap.extend_from_slice(&(packet.len() as u32).to_le_bytes());
        pcap.extend_from_slice(&(packet.len() as u32).to_le_bytes());
        pcap.extend_from_slice(packet);
    }

    fn null_ipv4_tcp(
        source_port: u16,
        destination_port: u16,
        sequence: u32,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&2u32.to_ne_bytes());
        let total_length = 20 + 20 + payload.len();
        packet.push(0x45);
        packet.push(0);
        packet.extend_from_slice(&(total_length as u16).to_be_bytes());
        packet.extend_from_slice(&[0; 5]);
        packet.push(6);
        packet.extend_from_slice(&[0; 2]);
        packet.extend_from_slice(&[127, 0, 0, 1]);
        packet.extend_from_slice(&[127, 0, 0, 1]);
        packet.extend_from_slice(&source_port.to_be_bytes());
        packet.extend_from_slice(&destination_port.to_be_bytes());
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(&0u32.to_be_bytes());
        packet.push(0x50);
        packet.push(0x18);
        packet.extend_from_slice(&65535u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }
}
