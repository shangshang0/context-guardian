use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_INTERVAL_MS: u64 = 1_000;
const DEFAULT_CONTEXT_TRIGGER_TOKENS: u64 = 200_000;
const DEFAULT_RECOVERY_TOKENS: u64 = 100_000;
const DEFAULT_LARGE_TOOL_OUTPUT_BYTES: usize = 160_000;
const DEFAULT_CC_SWITCH_URL: &str = "http://127.0.0.1:15721/v1/chat/completions";
const DEFAULT_CC_SWITCH_MODEL: &str = "feature/gpt-5.6-sol";
const DEFAULT_CC_SWITCH_CHUNK_TARGET_TOKENS: usize = 120_000;
const CC_SWITCH_MAX_REDUCE_ROUNDS: usize = 4;
const QUIET_DELAY_MS: u64 = 250;

#[derive(Debug)]
struct Config {
    thread_id: String,
    rollout: PathBuf,
    state_db: PathBuf,
    goals_db: PathBuf,
    interval: Duration,
    once: bool,
    status: bool,
    backup_dir: PathBuf,
    context_trigger_tokens: u64,
    recovery_tokens: u64,
    large_tool_output_bytes: usize,
    cc_switch_url: String,
    cc_switch_model: String,
    cc_switch_summary: bool,
    cc_switch_chunk_target_tokens: usize,
}

#[derive(Debug, Default)]
struct CleanStats {
    pruned_image_outputs: usize,
    pruned_user_image_attachments: usize,
    pruned_large_tool_outputs: usize,
    removed_data_uri_bytes: usize,
    removed_large_output_bytes: usize,
    dropped_context_alert_lines: usize,
    folded_obsolete_lines: usize,
    normalized_token_counts: usize,
    normalized_goal_token_counts: usize,
    backup_path: Option<PathBuf>,
}

#[derive(Debug, Default)]
struct DbStats {
    state_thread_rows: usize,
    goal_rows: usize,
}

#[derive(Debug, Clone)]
struct Record {
    line: String,
    newline: String,
    value: Value,
}

#[derive(Debug, Clone, Default)]
struct ToolCallInfo {
    name: String,
    image_path: Option<String>,
    arguments_preview: Option<String>,
}

fn main() {
    let config = match Config::from_args() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    if let Err(error) = validate_scope(&config) {
        eprintln!("scope validation failed: {error}");
        std::process::exit(2);
    }

    if config.status {
        if let Err(error) = print_status(&config) {
            eprintln!("status failed: {error}");
            std::process::exit(1);
        }
        return;
    }

    if config.once {
        let stats = match clean_rollout(&config) {
            Ok(stats) => stats,
            Err(error) => {
                eprintln!("clean failed: {error}");
                std::process::exit(1);
            }
        };
        log_stats(&stats);
        match repair_databases(&config) {
            Ok(stats) => log_db_stats(&stats),
            Err(error) => {
                eprintln!("db repair failed: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    println!(
        "context-guardian watching thread={} rollout={} trigger_tokens={}",
        config.thread_id,
        config.rollout.display(),
        config.context_trigger_tokens
    );

    let mut last_seen: Option<(u64, u64)> = None;
    loop {
        match file_fingerprint(&config.rollout) {
            Ok(fingerprint) => {
                if Some(fingerprint) != last_seen {
                    thread::sleep(Duration::from_millis(QUIET_DELAY_MS));
                    match clean_rollout(&config) {
                        Ok(stats) => {
                            if stats.changed() {
                                log_stats(&stats);
                            }
                            last_seen = file_fingerprint(&config.rollout).ok();
                        }
                        Err(error) => {
                            eprintln!("clean failed: {error}");
                            last_seen = None;
                        }
                    }
                }
                match repair_databases(&config) {
                    Ok(stats) => {
                        if stats.changed() {
                            log_db_stats(&stats);
                        }
                    }
                    Err(error) => eprintln!("db repair failed: {error}"),
                }
            }
            Err(error) => {
                eprintln!("stat failed for {}: {error}", config.rollout.display());
                last_seen = None;
                match repair_databases(&config) {
                    Ok(stats) => {
                        if stats.changed() {
                            log_db_stats(&stats);
                        }
                    }
                    Err(error) => eprintln!("db repair failed: {error}"),
                }
            }
        }
        thread::sleep(config.interval);
    }
}

impl Config {
    fn from_args() -> Result<Self, String> {
        let codex_home = codex_home()?;
        let mut config = Config {
            thread_id: String::new(),
            rollout: PathBuf::new(),
            state_db: codex_home.join("state_5.sqlite"),
            goals_db: codex_home.join("goals_1.sqlite"),
            interval: Duration::from_millis(DEFAULT_INTERVAL_MS),
            once: false,
            status: false,
            backup_dir: codex_home.join("context-guardian/backups"),
            context_trigger_tokens: DEFAULT_CONTEXT_TRIGGER_TOKENS,
            recovery_tokens: DEFAULT_RECOVERY_TOKENS,
            large_tool_output_bytes: DEFAULT_LARGE_TOOL_OUTPUT_BYTES,
            cc_switch_url: DEFAULT_CC_SWITCH_URL.to_string(),
            cc_switch_model: DEFAULT_CC_SWITCH_MODEL.to_string(),
            cc_switch_summary: false,
            cc_switch_chunk_target_tokens: DEFAULT_CC_SWITCH_CHUNK_TARGET_TOKENS,
        };

        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--thread-id" => config.thread_id = next_value(&mut args, "--thread-id")?,
                "--rollout" => config.rollout = PathBuf::from(next_value(&mut args, "--rollout")?),
                "--state-db" => {
                    config.state_db = PathBuf::from(next_value(&mut args, "--state-db")?)
                }
                "--goals-db" => {
                    config.goals_db = PathBuf::from(next_value(&mut args, "--goals-db")?)
                }
                "--interval-ms" => {
                    let value = next_value(&mut args, "--interval-ms")?;
                    let millis = value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid --interval-ms value: {value}"))?;
                    config.interval = Duration::from_millis(millis);
                }
                "--backup-dir" => {
                    config.backup_dir = PathBuf::from(next_value(&mut args, "--backup-dir")?)
                }
                "--context-trigger-tokens" => {
                    let value = next_value(&mut args, "--context-trigger-tokens")?;
                    config.context_trigger_tokens = value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid --context-trigger-tokens value: {value}"))?;
                }
                "--recovery-tokens" => {
                    let value = next_value(&mut args, "--recovery-tokens")?;
                    config.recovery_tokens = value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid --recovery-tokens value: {value}"))?;
                }
                "--large-tool-output-bytes" => {
                    let value = next_value(&mut args, "--large-tool-output-bytes")?;
                    config.large_tool_output_bytes = value
                        .parse::<usize>()
                        .map_err(|_| format!("invalid --large-tool-output-bytes value: {value}"))?;
                }
                "--cc-switch-url" => {
                    config.cc_switch_url = next_value(&mut args, "--cc-switch-url")?
                }
                "--cc-switch-model" => {
                    config.cc_switch_model = next_value(&mut args, "--cc-switch-model")?
                }
                "--cc-switch-chunk-target-tokens" => {
                    let value = next_value(&mut args, "--cc-switch-chunk-target-tokens")?;
                    config.cc_switch_chunk_target_tokens =
                        value.parse::<usize>().map_err(|_| {
                            format!("invalid --cc-switch-chunk-target-tokens value: {value}")
                        })?;
                }
                "--disable-cc-switch-summary" => config.cc_switch_summary = false,
                "--enable-cc-switch-summary" => config.cc_switch_summary = true,
                "--once" => config.once = true,
                "--status" => config.status = true,
                "--help" | "-h" => return Err(help_text()),
                other => return Err(format!("unknown argument: {other}\n{}", help_text())),
            }
        }
        if config.thread_id.is_empty() {
            return Err(format!("--thread-id is required\n{}", help_text()));
        }
        if config.rollout.as_os_str().is_empty() {
            config.rollout = discover_rollout(&config.state_db, &config.thread_id)
                .map_err(|error| format!("could not discover rollout: {error}"))?;
        }
        Ok(config)
    }
}

fn codex_home() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(path));
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| "set CODEX_HOME, HOME, or USERPROFILE".to_string())?;
    Ok(PathBuf::from(home).join(".codex"))
}

fn discover_rollout(state_db: &Path, thread_id: &str) -> io::Result<PathBuf> {
    let connection = open_database(state_db)?;
    let rollout: String = connection
        .query_row(
            "SELECT rollout_path FROM threads WHERE id = ?1",
            params![thread_id],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    Ok(PathBuf::from(rollout))
}

impl CleanStats {
    fn changed(&self) -> bool {
        self.pruned_image_outputs > 0
            || self.pruned_user_image_attachments > 0
            || self.pruned_large_tool_outputs > 0
            || self.dropped_context_alert_lines > 0
            || self.folded_obsolete_lines > 0
            || self.normalized_token_counts > 0
            || self.normalized_goal_token_counts > 0
    }

    fn needs_backup(&self) -> bool {
        self.pruned_image_outputs > 0
            || self.pruned_user_image_attachments > 0
            || self.pruned_large_tool_outputs > 0
            || self.folded_obsolete_lines > 0
            || self.normalized_token_counts > 0
            || self.normalized_goal_token_counts > 0
    }
}

impl DbStats {
    fn changed(&self) -> bool {
        self.state_thread_rows > 0 || self.goal_rows > 0
    }
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn help_text() -> String {
    "Usage: context-guardian --thread-id ID [--status|--once] [--rollout PATH] [--state-db PATH] [--goals-db PATH] [--interval-ms N] [--context-trigger-tokens N] [--recovery-tokens N] [--large-tool-output-bytes N] [--enable-cc-switch-summary] [--cc-switch-url URL] [--cc-switch-model MODEL] [--cc-switch-chunk-target-tokens N]".to_string()
}

fn validate_scope(config: &Config) -> io::Result<()> {
    if !is_safe_thread_id(&config.thread_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "thread id must be a safe UUID-like Codex thread id",
        ));
    }

    if config.recovery_tokens >= config.context_trigger_tokens {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--recovery-tokens must be lower than --context-trigger-tokens",
        ));
    }

    let rollout = config.rollout.to_string_lossy();
    if !rollout.contains(&config.thread_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "rollout path does not contain the configured thread id",
        ));
    }

    let canonical = fs::canonicalize(&config.rollout)?;
    if !canonical.to_string_lossy().contains(&config.thread_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "canonical rollout path does not contain the configured thread id",
        ));
    }
    validate_db_path(&config.state_db, "state_5.sqlite")?;
    if config.goals_db.exists() {
        validate_db_path(&config.goals_db, "goals_1.sqlite")?;
    }
    Ok(())
}

fn is_safe_thread_id(thread_id: &str) -> bool {
    !thread_id.is_empty()
        && thread_id.len() <= 80
        && thread_id
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() || ch == '-')
}

fn validate_db_path(path: &Path, expected_name: &str) -> io::Result<()> {
    let canonical = fs::canonicalize(path)?;
    let Some(file_name) = canonical.file_name().and_then(|name| name.to_str()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "database path has no file name",
        ));
    };
    if file_name != expected_name {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("database path must end with {expected_name}"),
        ));
    }
    Ok(())
}

fn file_fingerprint(path: &Path) -> io::Result<(u64, u64)> {
    let metadata = fs::metadata(path)?;
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    Ok((metadata.len(), modified))
}

fn clean_rollout(config: &Config) -> io::Result<CleanStats> {
    let before = fs::metadata(&config.rollout)?;
    let contents = fs::read_to_string(&config.rollout)?;
    let records = parse_records(&contents)?;
    let analysis = analyze_records(&records, config.context_trigger_tokens);
    let mut stats = CleanStats::default();
    let mut output = String::with_capacity(contents.len());

    for (index, record) in records.iter().enumerate() {
        if should_fold_line(index, record, analysis.fold_start_index) {
            stats.folded_obsolete_lines += 1;
            continue;
        }

        if is_high_token_count(&record.value, config.context_trigger_tokens)
            || is_recoverable_task_error(&record.value)
        {
            stats.dropped_context_alert_lines += 1;
            continue;
        }

        let mut value = record.value.clone();
        if normalize_token_count_event(
            &mut value,
            &records,
            config.context_trigger_tokens,
            config.recovery_tokens,
        ) {
            stats.normalized_token_counts += 1;
        }
        if normalize_thread_goal_token_count(
            &mut value,
            config.context_trigger_tokens,
            config.recovery_tokens,
        ) {
            stats.normalized_goal_token_counts += 1;
        }

        let (image_placeholders, removed_user_image_bytes) =
            scrub_user_message_images(&mut value, &config.thread_id);
        if image_placeholders > 0 {
            stats.pruned_user_image_attachments += image_placeholders;
            stats.removed_data_uri_bytes += removed_user_image_bytes;
        }

        if let Some(removed) =
            prune_inline_image_output(&mut value, &analysis.tool_calls, &config.thread_id)
        {
            stats.pruned_image_outputs += 1;
            stats.removed_data_uri_bytes += removed;
        }

        let removed_inline_images =
            scrub_inline_images(&mut value, &config.thread_id, "unknown image path");
        if removed_inline_images > 0 {
            stats.pruned_image_outputs += 1;
            stats.removed_data_uri_bytes += removed_inline_images;
        }

        if let Some(removed) = prune_large_tool_output(&mut value, &analysis.tool_calls, config) {
            stats.pruned_large_tool_outputs += 1;
            stats.removed_large_output_bytes += removed;
        }

        if value == record.value {
            output.push_str(&record.line);
        } else {
            output.push_str(&serialize_json_line(&value)?);
        }
        output.push_str(&record.newline);
    }

    if !stats.changed() {
        return Ok(stats);
    }

    let after_read = fs::metadata(&config.rollout)?;
    if before.len() != after_read.len() || before.modified()? != after_read.modified()? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "rollout changed while reading; will retry on next poll",
        ));
    }

    if stats.needs_backup() {
        fs::create_dir_all(&config.backup_dir)?;
        let backup_path = backup_path(&config.backup_dir, &config.rollout);
        fs::copy(&config.rollout, &backup_path)?;
        stats.backup_path = Some(backup_path);
    }
    write_same_inode(&config.rollout, output.as_bytes())?;
    Ok(stats)
}

fn parse_records(contents: &str) -> io::Result<Vec<Record>> {
    let mut records = Vec::new();
    for (line_index, raw_line) in contents.split_inclusive('\n').enumerate() {
        let (line, newline) = split_newline(raw_line);
        if line.trim().is_empty() {
            continue;
        }
        records.push(Record {
            line: line.to_string(),
            newline: newline.to_string(),
            value: parse_json_line(line, line_index + 1)?,
        });
    }
    Ok(records)
}

#[derive(Debug, Default)]
struct RolloutAnalysis {
    tool_calls: HashMap<String, ToolCallInfo>,
    fold_start_index: Option<usize>,
    trigger_start_index: Option<usize>,
}

fn analyze_records(records: &[Record], context_trigger_tokens: u64) -> RolloutAnalysis {
    let mut analysis = RolloutAnalysis::default();
    let mut latest_compacted_index = None;

    for (index, record) in records.iter().enumerate() {
        if record.value.get("type").and_then(Value::as_str) == Some("compacted") {
            latest_compacted_index = Some(index);
        }
        if let Some((call_id, info)) = tool_call_info(&record.value) {
            analysis.tool_calls.insert(call_id, info);
        }
    }

    let search_start = latest_compacted_index.unwrap_or(0);
    let trigger_index =
        records
            .iter()
            .enumerate()
            .skip(search_start)
            .find_map(|(index, record)| {
                (is_high_token_count(&record.value, context_trigger_tokens)
                    || is_recoverable_task_error(&record.value))
                .then_some(index)
            });

    analysis.trigger_start_index = trigger_index;
    analysis.fold_start_index = match (trigger_index, latest_compacted_index) {
        (Some(trigger), Some(compacted)) if compacted > 0 && trigger > compacted => Some(compacted),
        _ => None,
    };
    analysis
}

fn should_fold_line(index: usize, record: &Record, fold_start_index: Option<usize>) -> bool {
    let Some(fold_start_index) = fold_start_index else {
        return false;
    };
    index < fold_start_index && !is_session_meta(&record.value)
}

fn is_session_meta(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("session_meta")
}

fn split_newline(raw_line: &str) -> (&str, &str) {
    raw_line
        .strip_suffix('\n')
        .map(|line| (line, "\n"))
        .unwrap_or((raw_line, ""))
}

fn parse_json_line(line: &str, line_number: usize) -> io::Result<Value> {
    serde_json::from_str(line).map_err(|error| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("line {line_number} is not complete JSON yet: {error}"),
        )
    })
}

fn serialize_json_line(value: &Value) -> io::Result<String> {
    serde_json::to_string(value).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn write_same_inode(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).truncate(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn backup_path(backup_dir: &Path, rollout: &Path) -> PathBuf {
    let basename = rollout
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("rollout.jsonl");
    let stamp = unix_seconds();
    backup_dir.join(format!("{basename}.before-janitor-{stamp}.jsonl"))
}

fn log_stats(stats: &CleanStats) {
    println!(
        "pruned_image_outputs={} pruned_user_image_attachments={} pruned_large_tool_outputs={} removed_data_uri_bytes={} removed_large_output_bytes={} dropped_context_alert_lines={} folded_obsolete_lines={} normalized_token_counts={} normalized_goal_token_counts={} backup={}",
        stats.pruned_image_outputs,
        stats.pruned_user_image_attachments,
        stats.pruned_large_tool_outputs,
        stats.removed_data_uri_bytes,
        stats.removed_large_output_bytes,
        stats.dropped_context_alert_lines,
        stats.folded_obsolete_lines,
        stats.normalized_token_counts,
        stats.normalized_goal_token_counts,
        stats
            .backup_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "none".to_string())
    );
}

fn log_db_stats(stats: &DbStats) {
    println!(
        "db_state_thread_rows={} db_goal_rows={}",
        stats.state_thread_rows, stats.goal_rows
    );
}

fn repair_databases(config: &Config) -> io::Result<DbStats> {
    let state_thread_rows = repair_state_db(config)?;
    let goal_rows = repair_goals_db(config)?;
    Ok(DbStats {
        state_thread_rows,
        goal_rows,
    })
}

fn repair_state_db(config: &Config) -> io::Result<usize> {
    let now = unix_seconds();
    let now_ms = now.saturating_mul(1_000);
    let mut connection = open_database(&config.state_db)?;
    let transaction = connection.transaction().map_err(sqlite_error)?;
    let changed = transaction
        .execute(
            "UPDATE threads
             SET tokens_used = ?1,
                 updated_at = MAX(updated_at, ?2),
                 updated_at_ms = MAX(updated_at_ms, ?3),
                 recency_at = MAX(recency_at, ?2),
                 recency_at_ms = MAX(recency_at_ms, ?3)
             WHERE id = ?4 AND tokens_used >= ?5",
            params![
                config.recovery_tokens,
                now,
                now_ms,
                config.thread_id,
                config.context_trigger_tokens
            ],
        )
        .map_err(sqlite_error)?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(changed)
}

fn repair_goals_db(config: &Config) -> io::Result<usize> {
    if !config.goals_db.exists() {
        return Ok(0);
    }
    let now_ms = unix_seconds().saturating_mul(1_000);
    let mut connection = open_database(&config.goals_db)?;
    let transaction = connection.transaction().map_err(sqlite_error)?;
    let changed = transaction
        .execute(
            "UPDATE thread_goals
             SET tokens_used = ?1,
                 updated_at_ms = MAX(updated_at_ms, ?2)
             WHERE thread_id = ?3 AND tokens_used >= ?4",
            params![
                config.recovery_tokens,
                now_ms,
                config.thread_id,
                config.context_trigger_tokens
            ],
        )
        .map_err(sqlite_error)?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(changed)
}

fn open_database(path: &Path) -> io::Result<Connection> {
    let connection = Connection::open(path).map_err(sqlite_error)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(sqlite_error)?;
    Ok(connection)
}

fn sqlite_error(error: rusqlite::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, error)
}

fn print_status(config: &Config) -> io::Result<()> {
    let connection = open_database(&config.state_db)?;
    let (tokens_used, model, title): (u64, String, String) = connection
        .query_row(
            "SELECT tokens_used, COALESCE(model, ''), COALESCE(title, '') FROM threads WHERE id = ?1",
            params![config.thread_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(sqlite_error)?;
    let metadata = fs::metadata(&config.rollout)?;
    println!(
        "{}",
        json!({
            "thread_id": config.thread_id,
            "rollout": config.rollout,
            "rollout_bytes": metadata.len(),
            "tokens_used": tokens_used,
            "trigger_tokens": config.context_trigger_tokens,
            "recovery_tokens": config.recovery_tokens,
            "over_threshold": tokens_used >= config.context_trigger_tokens,
            "model": model,
            "title": title,
        })
    );
    Ok(())
}

fn tool_call_info(value: &Value) -> Option<(String, ToolCallInfo)> {
    let payload = value.get("payload")?;
    if payload.get("type")?.as_str()? != "function_call" {
        return None;
    }
    let call_id = payload.get("call_id")?.as_str()?.to_string();
    let name = payload.get("name")?.as_str()?.to_string();
    let arguments_preview = payload
        .get("arguments")
        .map(|arguments| preview(&argument_value_to_string(arguments), 400));
    let image_path = (name == "view_image")
        .then(|| argument_path(payload.get("arguments")?))
        .flatten();
    Some((
        call_id,
        ToolCallInfo {
            name,
            image_path,
            arguments_preview,
        },
    ))
}

fn argument_value_to_string(arguments: &Value) -> String {
    match arguments {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn argument_path(arguments: &Value) -> Option<String> {
    match arguments {
        Value::String(arguments) => serde_json::from_str::<Value>(arguments)
            .ok()
            .and_then(|value| value.get("path")?.as_str().map(ToOwned::to_owned)),
        Value::Object(_) => arguments.get("path")?.as_str().map(ToOwned::to_owned),
        _ => None,
    }
}

fn prune_inline_image_output(
    value: &mut Value,
    tool_calls: &HashMap<String, ToolCallInfo>,
    thread_id: &str,
) -> Option<usize> {
    let payload = value.get_mut("payload")?.as_object_mut()?;
    if payload.get("type")?.as_str()? != "function_call_output" {
        return None;
    }

    let call_id = payload
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown-call")
        .to_string();
    let output = payload.get_mut("output")?;
    let image_path = tool_calls
        .get(&call_id)
        .and_then(|info| info.image_path.as_deref())
        .unwrap_or("unknown image path");
    let removed = scrub_inline_images(output, thread_id, image_path);
    (removed > 0).then_some(removed)
}

fn scrub_inline_images(value: &mut Value, thread_id: &str, image_path: &str) -> usize {
    match value {
        Value::String(text) => {
            let Some((scrubbed, removed)) = scrub_data_image_text(text, thread_id, image_path)
            else {
                return 0;
            };
            *text = scrubbed;
            removed
        }
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("input_image") {
                let Some(image_url) = map.get("image_url").and_then(Value::as_str) else {
                    return 0;
                };
                let replacement = if image_url.starts_with("data:image") {
                    let removed = image_url.len();
                    Some((
                        image_placeholder(
                            thread_id,
                            image_path,
                            data_image_mime(image_url),
                            removed,
                        ),
                        removed,
                    ))
                } else if is_image_placeholder(image_url) {
                    Some((image_url.to_string(), 1))
                } else {
                    None
                };
                if let Some((text, removed)) = replacement {
                    map.clear();
                    map.insert("type".to_string(), Value::String("input_text".to_string()));
                    map.insert("text".to_string(), Value::String(text));
                    return removed;
                }
            }
            map.values_mut()
                .map(|value| scrub_inline_images(value, thread_id, image_path))
                .sum()
        }
        Value::Array(items) => items
            .iter_mut()
            .map(|value| scrub_inline_images(value, thread_id, image_path))
            .sum(),
        _ => 0,
    }
}

fn is_image_placeholder(value: &str) -> bool {
    value.starts_with("[context-guardian:inline-image-placeholder ")
        || value.starts_with("[codex-context-janitor:inline-image-placeholder ")
}

fn scrub_data_image_text(text: &str, thread_id: &str, image_path: &str) -> Option<(String, usize)> {
    let spans = data_image_spans(text);
    if spans.is_empty() {
        return None;
    }

    let mut scrubbed = String::with_capacity(text.len().min(4096));
    let mut cursor = 0usize;
    let mut removed = 0usize;
    for (start, end) in spans {
        scrubbed.push_str(&text[cursor..start]);
        let data_uri = &text[start..end];
        removed += data_uri.len();
        scrubbed.push_str(&image_placeholder(
            thread_id,
            image_path,
            data_image_mime(data_uri),
            data_uri.len(),
        ));
        cursor = end;
    }
    scrubbed.push_str(&text[cursor..]);
    Some((scrubbed, removed))
}

fn image_placeholder(
    thread_id: &str,
    image_path: &str,
    mime: String,
    removed_bytes: usize,
) -> String {
    format!(
        "[context-guardian:inline-image-placeholder thread={thread_id} mime={mime} removed_bytes={removed_bytes} original_file={image_path}]"
    )
}

fn data_image_mime(data_uri: &str) -> String {
    data_uri
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(';').or_else(|| rest.split_once(',')))
        .map(|(mime, _)| mime.to_string())
        .filter(|mime| mime.starts_with("image/"))
        .unwrap_or_else(|| "image/*".to_string())
}

fn scrub_user_message_images(value: &mut Value, thread_id: &str) -> (usize, usize) {
    if value.get("type").and_then(Value::as_str) != Some("event_msg") {
        return (0, 0);
    }
    let Some(payload) = value.get_mut("payload").and_then(Value::as_object_mut) else {
        return (0, 0);
    };
    if payload.get("type").and_then(Value::as_str) != Some("user_message") {
        return (0, 0);
    }

    let mut placeholders = Vec::new();
    let mut removed_bytes = 0usize;
    for field in ["images", "local_images"] {
        let Some(items) = payload.get_mut(field).and_then(Value::as_array_mut) else {
            continue;
        };
        if items.is_empty() {
            continue;
        }
        for item in items.iter() {
            let (placeholder, bytes) = user_image_placeholder(thread_id, field, item);
            placeholders.push(Value::String(placeholder));
            removed_bytes += bytes;
        }
        items.clear();
    }

    if placeholders.is_empty() {
        return (0, 0);
    }

    let count = placeholders.len();
    payload.insert("image_placeholders".to_string(), Value::Array(placeholders));
    append_user_image_placeholder_note(payload, count);
    (count, removed_bytes)
}

fn user_image_placeholder(thread_id: &str, field: &str, item: &Value) -> (String, usize) {
    let reference = image_reference_preview(item);
    let bytes = estimate_image_reference_bytes(item, &reference);
    (
        format!(
            "[context-guardian:user-image-placeholder thread={thread_id} field={field} estimated_removed_bytes={bytes} original={reference}]"
        ),
        bytes,
    )
}

fn image_reference_preview(item: &Value) -> String {
    match item {
        Value::String(text) => preview(text, 300),
        other => preview(&serde_json::to_string(other).unwrap_or_default(), 300),
    }
}

fn estimate_image_reference_bytes(item: &Value, reference: &str) -> usize {
    let inline_bytes = inline_data_image_bytes(item);
    if inline_bytes > 0 {
        return inline_bytes;
    }

    match item {
        Value::String(path) => fs::metadata(Path::new(path))
            .map(|metadata| metadata.len() as usize)
            .unwrap_or_else(|_| path.len()),
        other => serde_json::to_string(other)
            .map(|text| text.len())
            .unwrap_or_else(|_| reference.len()),
    }
}

fn append_user_image_placeholder_note(payload: &mut Map<String, Value>, count: usize) {
    let note = format!(
        "\n\n[context-guardian removed {count} image attachment(s) from this historical user message. The image references are preserved in payload.image_placeholders so future context rebuilds do not inline large image bodies.]"
    );
    match payload.get_mut("message") {
        Some(Value::String(message)) if !message.contains("payload.image_placeholders") => {
            message.push_str(&note);
        }
        Some(Value::String(_)) => {}
        _ => {
            payload.insert(
                "message".to_string(),
                Value::String(note.trim().to_string()),
            );
        }
    }
}

fn prune_large_tool_output(
    value: &mut Value,
    tool_calls: &HashMap<String, ToolCallInfo>,
    config: &Config,
) -> Option<usize> {
    let payload = value.get_mut("payload")?.as_object_mut()?;
    if payload.get("type")?.as_str()? != "function_call_output" {
        return None;
    }
    let call_id = payload
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown-call")
        .to_string();
    let output = payload.get_mut("output")?;
    if inline_data_image_bytes(output) > 0 {
        return None;
    }

    let encoded_len = encoded_json_len(output);
    if encoded_len < config.large_tool_output_bytes {
        return None;
    }

    let tool_info = tool_calls.get(&call_id).cloned().unwrap_or_default();
    let summary = summarize_large_output(output, &tool_info, config);
    *output = Value::String(match summary {
        Some(summary) => format!(
            "Large tool output summarized by context-guardian via CC Switch because the original tool result was {encoded_len} bytes and could exceed the model context window. Tool: {}. Arguments: {}.\n\nSummary:\n{summary}",
            nonempty(&tool_info.name, "unknown-tool"),
            tool_info.arguments_preview.as_deref().unwrap_or("unknown arguments")
        ),
        None => format!(
            "Large tool output pruned by context-guardian because the original tool result was {encoded_len} bytes and could exceed the model context window. Tool: {}. Arguments: {}. Re-run the command or inspect the referenced file manually if the full output is needed.",
            nonempty(&tool_info.name, "unknown-tool"),
            tool_info.arguments_preview.as_deref().unwrap_or("unknown arguments")
        ),
    });
    Some(encoded_len)
}

fn inline_data_image_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => data_image_spans(text)
            .iter()
            .map(|(start, end)| end - start)
            .sum(),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| {
                let _ = key;
                inline_data_image_bytes(value)
            })
            .sum(),
        Value::Array(items) => items.iter().map(inline_data_image_bytes).sum(),
        _ => 0,
    }
}

fn data_image_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut offset = 0usize;
    while let Some(relative_start) = text[offset..].find("data:image") {
        let start = offset + relative_start;
        let rest = &text[start..];
        let end = rest
            .find(|ch: char| {
                ch == '"' || ch == '\'' || ch.is_whitespace() || ch == ')' || ch == ']'
            })
            .map(|relative_end| start + relative_end)
            .unwrap_or(text.len());
        spans.push((start, end));
        offset = end.max(start + "data:image".len());
    }
    spans
}

fn encoded_json_len(value: &Value) -> usize {
    serde_json::to_string(value)
        .map(|text| text.len())
        .unwrap_or_default()
}

fn summarize_large_output(
    value: &Value,
    tool_info: &ToolCallInfo,
    config: &Config,
) -> Option<String> {
    if !config.cc_switch_summary {
        return None;
    }
    let original = match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).ok()?,
    };
    map_reduce_summary(&original, tool_info, config)
}

fn map_reduce_summary(original: &str, tool_info: &ToolCallInfo, config: &Config) -> Option<String> {
    let target_tokens = config.cc_switch_chunk_target_tokens.max(8_000);
    let mut parts = chunk_by_estimated_tokens(original, target_tokens);
    if parts.is_empty() {
        return None;
    }

    let original_len = original.len();
    let original_chunks = parts.len();
    let mut round = 0usize;
    while parts.len() > 1
        || estimate_tokens(parts.first().map(String::as_str).unwrap_or_default()) > target_tokens
    {
        if round >= CC_SWITCH_MAX_REDUCE_ROUNDS {
            return Some(parts.join("\n\n"));
        }
        let part_count = parts.len();
        let mut summaries = Vec::with_capacity(part_count);
        for (index, part) in parts.iter().enumerate() {
            let prompt = format!(
                "Tool: {}\nArguments: {}\nOriginal output bytes: {original_len}\nCompression round: {}\nChunk: {} of {}\nChunk bytes: {}\n\nSummarize this chunk for Codex context recovery. Preserve concrete file paths, commands, errors, test results, and decisions. Do not invent content outside this chunk.\n\n{}",
                nonempty(&tool_info.name, "unknown-tool"),
                tool_info.arguments_preview.as_deref().unwrap_or("unknown arguments"),
                round + 1,
                index + 1,
                part_count,
                part.len(),
                part
            );
            summaries.push(cc_switch_summarize(&prompt, 700, config)?);
        }
        parts = chunk_by_estimated_tokens(
            &summaries.join("\n\n--- next chunk summary ---\n\n"),
            target_tokens,
        );
        round += 1;
    }

    let joined = parts.join("\n\n");
    let final_prompt = format!(
        "Tool: {}\nArguments: {}\nOriginal output bytes: {original_len}\nOriginal chunks: {original_chunks}\n\nCombine these chunk summaries into one concise recovery summary for Codex. Preserve actionable facts and paths.\n\n{}",
        nonempty(&tool_info.name, "unknown-tool"),
        tool_info.arguments_preview.as_deref().unwrap_or("unknown arguments"),
        joined
    );
    cc_switch_summarize(&final_prompt, 900, config).or(Some(joined))
}

fn cc_switch_summarize(prompt: &str, max_tokens: usize, config: &Config) -> Option<String> {
    let payload = json!({
        "model": config.cc_switch_model,
        "messages": [
            {
                "role": "system",
                "content": "You summarize oversized Codex context fragments for local recovery. Preserve concrete file paths, commands, errors, test results, and decisions. Be concise and factual."
            },
            { "role": "user", "content": prompt }
        ],
        "max_tokens": max_tokens
    });

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build();
    let response = agent
        .post(&config.cc_switch_url)
        .set("Content-Type", "application/json")
        .send_json(payload)
        .ok()?;
    let response_json: Value = response.into_json().ok()?;
    let summary = response_json
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()?
        .trim()
        .to_string();
    (!summary.is_empty()).then_some(summary)
}

fn chunk_by_estimated_tokens(input: &str, target_tokens: usize) -> Vec<String> {
    let target_tokens = target_tokens.max(1);
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_tokens = 0usize;

    for line in input.split_inclusive('\n') {
        let line_tokens = estimate_tokens(line);
        if !current.is_empty() && current_tokens + line_tokens > target_tokens {
            chunks.push(std::mem::take(&mut current));
            current_tokens = 0;
        }

        if line_tokens > target_tokens {
            for piece in split_long_piece(line, target_tokens) {
                if !current.is_empty() {
                    chunks.push(std::mem::take(&mut current));
                    current_tokens = 0;
                }
                chunks.push(piece);
            }
            continue;
        }

        current.push_str(line);
        current_tokens += line_tokens;
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn split_long_piece(input: &str, target_tokens: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut tokens = 0usize;
    for ch in input.chars() {
        let ch_tokens = estimate_char_tokens(ch);
        if !current.is_empty() && tokens + ch_tokens > target_tokens {
            pieces.push(std::mem::take(&mut current));
            tokens = 0;
        }
        current.push(ch);
        tokens += ch_tokens;
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

fn estimate_tokens(input: &str) -> usize {
    input
        .chars()
        .map(estimate_char_tokens)
        .sum::<usize>()
        .max(1)
}

fn estimate_char_tokens(ch: char) -> usize {
    if ch.is_ascii() {
        1
    } else {
        2
    }
}

fn is_high_token_count(value: &Value, context_trigger_tokens: u64) -> bool {
    if value.get("type").and_then(Value::as_str) != Some("event_msg") {
        return false;
    }
    let payload = match value.get("payload") {
        Some(payload) if payload.get("type").and_then(Value::as_str) == Some("token_count") => {
            payload
        }
        _ => return false,
    };
    observed_context_tokens(payload) >= context_trigger_tokens
}

fn observed_context_tokens(token_count_payload: &Value) -> u64 {
    observed_usage_tokens(&token_count_payload["info"]["last_token_usage"]).max(
        observed_usage_tokens(&token_count_payload["info"]["total_token_usage"]),
    )
}

fn observed_usage_tokens(usage: &Value) -> u64 {
    let input = usage["input_tokens"].as_u64().unwrap_or_default();
    let total = usage["total_tokens"].as_u64().unwrap_or_default();
    input.max(total)
}

fn normalize_token_count_event(
    value: &mut Value,
    records: &[Record],
    context_trigger_tokens: u64,
    recovery_tokens: u64,
) -> bool {
    if value.get("type").and_then(Value::as_str) != Some("event_msg") {
        return false;
    }
    let Some(payload) = value.get_mut("payload") else {
        return false;
    };
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return false;
    }
    if observed_context_tokens(payload) < context_trigger_tokens {
        return false;
    }

    let estimated_tokens = estimate_rollout_tokens(records, recovery_tokens);
    let Some(info) = payload.get_mut("info").and_then(Value::as_object_mut) else {
        return false;
    };
    info.insert(
        "total_token_usage".to_string(),
        json!({
            "input_tokens": estimated_tokens,
            "cached_input_tokens": 0,
            "cache_write_input_tokens": 0,
            "output_tokens": 0,
            "reasoning_output_tokens": 0,
            "total_tokens": estimated_tokens
        }),
    );
    true
}

fn normalize_thread_goal_token_count(
    value: &mut Value,
    context_trigger_tokens: u64,
    recovery_tokens: u64,
) -> bool {
    if value.get("type").and_then(Value::as_str) != Some("event_msg") {
        return false;
    }
    let Some(payload) = value.get_mut("payload") else {
        return false;
    };
    if payload.get("type").and_then(Value::as_str) != Some("thread_goal_updated") {
        return false;
    }
    let Some(goal) = payload.get_mut("goal").and_then(Value::as_object_mut) else {
        return false;
    };
    let tokens_used = goal
        .get("tokensUsed")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    if tokens_used < context_trigger_tokens {
        return false;
    }
    goal.insert("tokensUsed".to_string(), json!(recovery_tokens));
    true
}

fn estimate_rollout_tokens(records: &[Record], recovery_tokens: u64) -> u64 {
    let bytes: usize = records.iter().map(|record| record.line.len()).sum();
    ((bytes / 4).max(1) as u64).min(recovery_tokens)
}

fn is_recoverable_task_error(value: &Value) -> bool {
    if value.get("type").and_then(Value::as_str) != Some("event_msg") {
        return false;
    }
    let payload = match value.get("payload") {
        Some(payload) if payload.get("type").and_then(Value::as_str) == Some("task_complete") => {
            payload
        }
        _ => return false,
    };
    let Some(error) = payload.get("error") else {
        return false;
    };
    let error_info = error
        .get("codex_error_info")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    error_info.contains("context_window_exceeded")
        || message.contains("context window")
        || message.contains("exceeds the context")
        || message.contains("input exceeds")
        || (message.contains("image_url")
            && (message.contains("invalid format") || message.contains("valid url")))
}

fn preview(input: &str, max_chars: usize) -> String {
    let mut output = input.chars().take(max_chars).collect::<String>();
    if input.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn nonempty<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
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
    use serde_json::json;

    const TEST_THREAD_ID: &str = "019f7193-7201-7a91-a2c7-c2653f4b6c78";

    #[test]
    fn extracts_view_image_path_from_string_arguments() {
        let value = json!({
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "view_image",
                "arguments": "{\"detail\":\"high\",\"path\":\"/tmp/page.png\"}",
                "call_id": "call_1"
            }
        });

        let (_, info) = tool_call_info(&value).unwrap();
        assert_eq!(info.image_path, Some("/tmp/page.png".to_string()));
    }

    #[test]
    fn prunes_structured_inline_image_outputs() {
        let image_url = "data:image/png;base64,abc";
        let mut value = json!({
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": [{"type": "input_image", "image_url": image_url, "detail": "high"}]
            }
        });
        let mut tool_calls = HashMap::new();
        tool_calls.insert(
            "call_1".to_string(),
            ToolCallInfo {
                name: "view_image".to_string(),
                image_path: Some("/tmp/page.png".to_string()),
                arguments_preview: None,
            },
        );

        assert_eq!(
            prune_inline_image_output(&mut value, &tool_calls, TEST_THREAD_ID),
            Some(image_url.len())
        );
        let output = &value["payload"]["output"];
        assert!(output.is_array());
        assert_eq!(output[0]["type"], "input_text");
        assert!(output[0].get("image_url").is_none());
        let text = output[0]["text"].as_str().unwrap();
        assert!(text.contains("inline-image-placeholder"));
        assert!(text.contains("/tmp/page.png"));
        assert!(text.contains("removed_bytes=25"));
    }

    #[test]
    fn migrates_legacy_invalid_image_placeholders() {
        let mut value = json!({
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": [{
                    "type": "input_image",
                    "image_url": "[codex-context-janitor:inline-image-placeholder thread=test mime=image/png removed_bytes=25 original_file=/tmp/page.png]",
                    "detail": "high"
                }]
            }
        });

        assert_eq!(
            prune_inline_image_output(&mut value, &HashMap::new(), TEST_THREAD_ID),
            Some(1)
        );
        let output = &value["payload"]["output"][0];
        assert_eq!(output["type"], "input_text");
        assert!(output.get("image_url").is_none());
        assert!(output["text"]
            .as_str()
            .unwrap()
            .contains("inline-image-placeholder"));
    }

    #[test]
    fn prunes_string_inline_image_outputs() {
        let mut value = json!({
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "literal text containing \"image_url\":\"data:image/png;base64,abc\""
            }
        });
        let tool_calls = HashMap::new();

        assert_eq!(
            prune_inline_image_output(&mut value, &tool_calls, TEST_THREAD_ID),
            Some("data:image/png;base64,abc".len())
        );
        let output = value["payload"]["output"].as_str().unwrap();
        assert!(output.contains("literal text containing"));
        assert!(output.contains("inline-image-placeholder"));
        assert!(!output.contains("data:image"));
    }

    #[test]
    fn scrubs_user_message_image_references_once() {
        let inline_image = "data:image/png;base64,abc";
        let mut value = json!({
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": "inspect these",
                "images": [inline_image],
                "local_images": ["/tmp/page.png"],
                "text_elements": []
            }
        });

        let (count, removed_bytes) = scrub_user_message_images(&mut value, TEST_THREAD_ID);

        assert_eq!(count, 2);
        assert!(removed_bytes >= inline_image.len() + "/tmp/page.png".len());
        assert_eq!(value["payload"]["images"], json!([]));
        assert_eq!(value["payload"]["local_images"], json!([]));
        let placeholders = value["payload"]["image_placeholders"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(placeholders.len(), 2);
        assert!(placeholders[0].as_str().unwrap().contains("field=images"));
        assert!(placeholders[1]
            .as_str()
            .unwrap()
            .contains("field=local_images"));
        assert!(value["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("payload.image_placeholders"));

        assert_eq!(
            scrub_user_message_images(&mut value, TEST_THREAD_ID),
            (0, 0)
        );
        assert_eq!(value["payload"]["image_placeholders"], json!(placeholders));
    }

    #[test]
    fn detects_context_alerts() {
        let value = json!({
            "type": "event_msg",
            "payload": {
                "type": "task_complete",
                "error": {
                    "message": "stream disconnected before completion: Your input exceeds the context window of this model.",
                    "codex_error_info": "other"
                }
            }
        });

        assert!(is_recoverable_task_error(&value));
    }

    #[test]
    fn detects_invalid_image_url_errors() {
        let value = json!({
            "type": "event_msg",
            "payload": {
                "type": "task_complete",
                "error": {
                    "message": "Invalid 'input[327].content[2].image_url'. Expected a valid URL, but got a value with an invalid format."
                }
            }
        });

        assert!(is_recoverable_task_error(&value));
    }

    #[test]
    fn detects_200k_token_trigger() {
        let value = json!({
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {"last_token_usage": {"input_tokens": 200000, "total_tokens": 200123}}
            }
        });

        assert!(is_high_token_count(&value, DEFAULT_CONTEXT_TRIGGER_TOKENS));
    }

    #[test]
    fn detects_inline_images_inside_string_outputs() {
        let value = json!("prefix data:image/png;base64,abcdef suffix");

        assert_eq!(
            inline_data_image_bytes(&value),
            "data:image/png;base64,abcdef".len()
        );
    }
}
