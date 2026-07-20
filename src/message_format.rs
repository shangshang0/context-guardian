use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub(crate) struct FormatIssue {
    pub(crate) record_index: usize,
    pub(crate) path: String,
    pub(crate) expected: String,
    pub(crate) actual: String,
    pub(crate) repaired: bool,
}

impl FormatIssue {
    pub(crate) fn as_json(&self) -> Value {
        json!({
            "line": self.record_index + 1,
            "path": self.path,
            "expected": self.expected,
            "actual": self.actual,
            "repaired": self.repaired,
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct MessageDiagnosis {
    pub(crate) checked_records: usize,
    pub(crate) issues: Vec<FormatIssue>,
    pub(crate) repaired_values: HashMap<usize, Value>,
}

impl MessageDiagnosis {
    pub(crate) fn repaired_records(&self) -> usize {
        self.repaired_values.len()
    }

    pub(crate) fn unrepaired_issues(&self) -> usize {
        self.issues.iter().filter(|issue| !issue.repaired).count()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LiveProbeResult {
    pub(crate) attempted: bool,
    pub(crate) succeeded: bool,
    pub(crate) status: String,
}

impl LiveProbeResult {
    pub(crate) fn not_requested() -> Self {
        Self {
            attempted: false,
            succeeded: false,
            status: "not_requested".to_string(),
        }
    }

    pub(crate) fn as_json(&self) -> Value {
        json!({
            "attempted": self.attempted,
            "succeeded": self.succeeded,
            "status": self.status,
        })
    }
}

pub(crate) fn is_task_error(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("event_msg")
        && value
            .get("payload")
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
            == Some("task_complete")
        && value
            .get("payload")
            .and_then(|payload| payload.get("error"))
            .is_some_and(|error| !error.is_null())
}

pub(crate) fn diagnose_records<'a>(
    values: impl IntoIterator<Item = &'a Value>,
) -> MessageDiagnosis {
    let mut diagnosis = MessageDiagnosis::default();
    for (record_index, original) in values.into_iter().enumerate() {
        let mut value = original.clone();
        let issue_start = diagnosis.issues.len();
        let checked = diagnose_record(&mut value, record_index, &mut diagnosis.issues);
        if checked {
            diagnosis.checked_records += 1;
        }
        if value != *original
            && diagnosis.issues[issue_start..]
                .iter()
                .any(|issue| issue.repaired)
        {
            diagnosis.repaired_values.insert(record_index, value);
        }
    }
    diagnosis
}

fn diagnose_record(value: &mut Value, record_index: usize, issues: &mut Vec<FormatIssue>) -> bool {
    match value.get("type").and_then(Value::as_str) {
        Some("compacted") => {
            diagnose_compacted(value, record_index, issues);
            true
        }
        Some("response_item") => diagnose_response_item(value, record_index, issues),
        Some("event_msg")
            if value
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str)
                == Some("user_message") =>
        {
            diagnose_user_message(value, record_index, issues);
            true
        }
        _ => false,
    }
}

fn diagnose_compacted(value: &mut Value, record_index: usize, issues: &mut Vec<FormatIssue>) {
    let Some(payload) = value.get_mut("payload").and_then(Value::as_object_mut) else {
        issue(
            issues,
            record_index,
            "payload",
            "object",
            value.get("payload"),
            false,
        );
        return;
    };

    if let Some(message) = payload.get_mut("message") {
        if !message.is_string() {
            let replacement = flatten_text(message);
            let repaired = replacement.is_some();
            issue(
                issues,
                record_index,
                "payload.message",
                "string",
                Some(message),
                repaired,
            );
            if let Some(replacement) = replacement {
                *message = Value::String(replacement);
            }
        }
    } else {
        issue(
            issues,
            record_index,
            "payload.message",
            "string",
            None,
            false,
        );
    }

    let Some(history) = payload.get_mut("replacement_history") else {
        issue(
            issues,
            record_index,
            "payload.replacement_history",
            "array of message objects",
            None,
            false,
        );
        return;
    };

    normalize_history_container(history, record_index, issues);
    let Some(messages) = history.as_array_mut() else {
        return;
    };
    if messages.is_empty() {
        issue(
            issues,
            record_index,
            "payload.replacement_history",
            "non-empty array of message objects",
            Some(history),
            false,
        );
        return;
    }
    for (index, message) in messages.iter_mut().enumerate() {
        diagnose_message(
            message,
            record_index,
            &format!("payload.replacement_history[{index}]"),
            issues,
        );
    }
}

fn normalize_history_container(
    history: &mut Value,
    record_index: usize,
    issues: &mut Vec<FormatIssue>,
) {
    let replacement = match history {
        Value::String(encoded) => serde_json::from_str::<Value>(encoded)
            .ok()
            .and_then(|value| {
                if value.is_array() {
                    Some(value)
                } else if value.is_object() {
                    Some(Value::Array(vec![value]))
                } else {
                    None
                }
            }),
        Value::Object(_) => Some(Value::Array(vec![history.clone()])),
        _ => None,
    };
    if history.is_array() {
        return;
    }
    let repaired = replacement.is_some();
    issue(
        issues,
        record_index,
        "payload.replacement_history",
        "array of message objects",
        Some(history),
        repaired,
    );
    if let Some(replacement) = replacement {
        *history = replacement;
    }
}

fn diagnose_response_item(
    value: &mut Value,
    record_index: usize,
    issues: &mut Vec<FormatIssue>,
) -> bool {
    let Some(payload) = value.get_mut("payload") else {
        issue(issues, record_index, "payload", "object", None, false);
        return true;
    };
    let payload_type = payload.get("type").and_then(Value::as_str);
    match payload_type {
        Some("message") => {
            diagnose_message(payload, record_index, "payload", issues);
            true
        }
        Some("function_call") => {
            diagnose_function_call(payload, record_index, issues);
            true
        }
        Some("function_call_output") | Some("custom_tool_call_output") => {
            diagnose_tool_output(payload, record_index, issues);
            true
        }
        _ => false,
    }
}

fn diagnose_message(
    message: &mut Value,
    record_index: usize,
    path: &str,
    issues: &mut Vec<FormatIssue>,
) {
    let Some(map) = message.as_object_mut() else {
        issue(
            issues,
            record_index,
            path,
            "message object",
            Some(message),
            false,
        );
        return;
    };

    match map.get("type").and_then(Value::as_str) {
        Some("message") => {}
        None if map.contains_key("role") && map.contains_key("content") => {
            issue(
                issues,
                record_index,
                &format!("{path}.type"),
                "message",
                map.get("type"),
                true,
            );
            map.insert("type".to_string(), Value::String("message".to_string()));
        }
        _ => {
            issue(
                issues,
                record_index,
                &format!("{path}.type"),
                "message",
                map.get("type"),
                false,
            );
            return;
        }
    }

    let role = match map.get("role").and_then(Value::as_str) {
        Some(role @ ("assistant" | "user" | "developer" | "system")) => role.to_string(),
        _ => {
            issue(
                issues,
                record_index,
                &format!("{path}.role"),
                "assistant, user, developer, or system",
                map.get("role"),
                false,
            );
            return;
        }
    };
    let expected_text_type = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    let Some(content) = map.get_mut("content") else {
        issue(
            issues,
            record_index,
            &format!("{path}.content"),
            "non-empty content array",
            None,
            false,
        );
        return;
    };
    normalize_content_container(
        content,
        record_index,
        &format!("{path}.content"),
        expected_text_type,
        issues,
    );
}

fn normalize_content_container(
    content: &mut Value,
    record_index: usize,
    path: &str,
    expected_text_type: &str,
    issues: &mut Vec<FormatIssue>,
) {
    let replacement = match content {
        Value::String(text) => Some(Value::Array(vec![text_item(
            expected_text_type,
            text.clone(),
        )])),
        Value::Object(_) => Some(Value::Array(vec![content.clone()])),
        _ => None,
    };
    if !content.is_array() {
        let repaired = replacement.is_some();
        issue(
            issues,
            record_index,
            path,
            "non-empty content array",
            Some(content),
            repaired,
        );
        if let Some(replacement) = replacement {
            *content = replacement;
        } else {
            return;
        }
    }
    let Some(items) = content.as_array_mut() else {
        return;
    };
    if items.is_empty() {
        issue(
            issues,
            record_index,
            path,
            "non-empty content array",
            Some(content),
            false,
        );
        return;
    }
    for (index, item) in items.iter_mut().enumerate() {
        normalize_content_item(
            item,
            record_index,
            &format!("{path}[{index}]"),
            expected_text_type,
            issues,
        );
    }
}

fn normalize_content_item(
    item: &mut Value,
    record_index: usize,
    path: &str,
    expected_text_type: &str,
    issues: &mut Vec<FormatIssue>,
) {
    if let Value::String(text) = item {
        let replacement = text_item(expected_text_type, text.clone());
        issue(
            issues,
            record_index,
            path,
            "typed content object",
            Some(item),
            true,
        );
        *item = replacement;
        return;
    }
    let Some(map) = item.as_object_mut() else {
        issue(
            issues,
            record_index,
            path,
            "typed content object",
            Some(item),
            false,
        );
        return;
    };

    let item_type = map.get("type").and_then(Value::as_str).map(str::to_string);
    match item_type.as_deref() {
        None if map.get("text").and_then(Value::as_str).is_some() => {
            issue(
                issues,
                record_index,
                &format!("{path}.type"),
                expected_text_type,
                None,
                true,
            );
            map.insert(
                "type".to_string(),
                Value::String(expected_text_type.to_string()),
            );
        }
        Some("text") => {
            issue(
                issues,
                record_index,
                &format!("{path}.type"),
                expected_text_type,
                map.get("type"),
                true,
            );
            map.insert(
                "type".to_string(),
                Value::String(expected_text_type.to_string()),
            );
        }
        Some("input_text") if expected_text_type == "output_text" => {
            issue(
                issues,
                record_index,
                &format!("{path}.type"),
                expected_text_type,
                map.get("type"),
                true,
            );
            map.insert(
                "type".to_string(),
                Value::String(expected_text_type.to_string()),
            );
        }
        Some("output_text") if expected_text_type == "input_text" => {
            issue(
                issues,
                record_index,
                &format!("{path}.type"),
                expected_text_type,
                map.get("type"),
                true,
            );
            map.insert(
                "type".to_string(),
                Value::String(expected_text_type.to_string()),
            );
        }
        Some("input_image") if expected_text_type == "input_text" => {
            if map.get("image_url").and_then(Value::as_str).is_none() {
                issue(
                    issues,
                    record_index,
                    &format!("{path}.image_url"),
                    "string",
                    map.get("image_url"),
                    false,
                );
            }
            return;
        }
        Some("refusal") if expected_text_type == "output_text" => {
            if map.get("refusal").and_then(Value::as_str).is_none() {
                issue(
                    issues,
                    record_index,
                    &format!("{path}.refusal"),
                    "string",
                    map.get("refusal"),
                    false,
                );
            }
            return;
        }
        Some(value) if value == expected_text_type => {}
        Some(_) | None => {
            issue(
                issues,
                record_index,
                &format!("{path}.type"),
                expected_text_type,
                map.get("type"),
                false,
            );
            return;
        }
    }

    if map.get("text").and_then(Value::as_str).is_none() {
        if let Some(text) = map
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            issue(
                issues,
                record_index,
                &format!("{path}.text"),
                "string",
                map.get("text"),
                true,
            );
            map.remove("content");
            map.insert("text".to_string(), Value::String(text));
        } else {
            issue(
                issues,
                record_index,
                &format!("{path}.text"),
                "string",
                map.get("text"),
                false,
            );
        }
    }
}

fn diagnose_function_call(payload: &mut Value, record_index: usize, issues: &mut Vec<FormatIssue>) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    require_nonempty_string(map, "call_id", record_index, "payload.call_id", issues);
    require_nonempty_string(map, "name", record_index, "payload.name", issues);
    let Some(arguments) = map.get_mut("arguments") else {
        issue(
            issues,
            record_index,
            "payload.arguments",
            "JSON encoded as a string",
            None,
            false,
        );
        return;
    };
    if !arguments.is_string() {
        let replacement = serde_json::to_string(arguments).ok();
        let repaired = replacement.is_some();
        issue(
            issues,
            record_index,
            "payload.arguments",
            "JSON encoded as a string",
            Some(arguments),
            repaired,
        );
        if let Some(replacement) = replacement {
            *arguments = Value::String(replacement);
        }
    }
}

fn diagnose_tool_output(payload: &mut Value, record_index: usize, issues: &mut Vec<FormatIssue>) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    require_nonempty_string(map, "call_id", record_index, "payload.call_id", issues);
    let Some(output) = map.get_mut("output") else {
        issue(
            issues,
            record_index,
            "payload.output",
            "string or typed content array",
            None,
            false,
        );
        return;
    };
    match output {
        Value::String(_) => {}
        Value::Array(_) => normalize_content_container(
            output,
            record_index,
            "payload.output",
            "input_text",
            issues,
        ),
        Value::Object(map) if map.contains_key("type") || map.contains_key("text") => {
            normalize_content_container(
                output,
                record_index,
                "payload.output",
                "input_text",
                issues,
            )
        }
        _ => {
            let replacement = serde_json::to_string(output).ok();
            let repaired = replacement.is_some();
            issue(
                issues,
                record_index,
                "payload.output",
                "string or typed content array",
                Some(output),
                repaired,
            );
            if let Some(replacement) = replacement {
                *output = Value::String(replacement);
            }
        }
    }
}

fn diagnose_user_message(value: &mut Value, record_index: usize, issues: &mut Vec<FormatIssue>) {
    let Some(payload) = value.get_mut("payload").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(message) = payload.get_mut("message") {
        if !message.is_string() {
            let replacement = flatten_text(message);
            let repaired = replacement.is_some();
            issue(
                issues,
                record_index,
                "payload.message",
                "string",
                Some(message),
                repaired,
            );
            if let Some(replacement) = replacement {
                *message = Value::String(replacement);
            }
        }
    } else {
        issue(
            issues,
            record_index,
            "payload.message",
            "string",
            None,
            false,
        );
    }
    for field in ["images", "local_images", "text_elements"] {
        if let Some(value) = payload.get_mut(field) {
            if !value.is_array() {
                let repaired = value.is_null();
                issue(
                    issues,
                    record_index,
                    &format!("payload.{field}"),
                    "array",
                    Some(value),
                    repaired,
                );
                if repaired {
                    *value = Value::Array(Vec::new());
                }
            }
        }
    }
}

fn require_nonempty_string(
    map: &Map<String, Value>,
    key: &str,
    record_index: usize,
    path: &str,
    issues: &mut Vec<FormatIssue>,
) {
    if map
        .get(key)
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        issue(
            issues,
            record_index,
            path,
            "non-empty string",
            map.get(key),
            false,
        );
    }
}

fn flatten_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let mut text = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(value) => text.push(value.clone()),
                    Value::Object(map) => {
                        text.push(map.get("text")?.as_str()?.to_string());
                    }
                    _ => return None,
                }
            }
            (!text.is_empty()).then(|| text.join("\n"))
        }
        Value::Object(map) => map.get("text")?.as_str().map(str::to_string),
        _ => None,
    }
}

fn text_item(item_type: &str, text: String) -> Value {
    json!({"type": item_type, "text": text})
}

fn issue(
    issues: &mut Vec<FormatIssue>,
    record_index: usize,
    path: &str,
    expected: &str,
    actual: Option<&Value>,
    repaired: bool,
) {
    issues.push(FormatIssue {
        record_index,
        path: path.to_string(),
        expected: expected.to_string(),
        actual: value_kind(actual).to_string(),
        repaired,
    });
}

fn value_kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

pub(crate) fn run_live_probe(command: &Path, timeout: Duration) -> LiveProbeResult {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let probe_dir = env::temp_dir().join(format!(
        "context-guardian-message-probe-{}-{stamp}",
        std::process::id()
    ));
    if let Err(error) = fs::create_dir(&probe_dir) {
        return LiveProbeResult {
            attempted: true,
            succeeded: false,
            status: format!("probe_setup_failed:{:?}", error.kind()),
        };
    }

    let child = Command::new(command)
        .args([
            "exec",
            "--ephemeral",
            "--json",
            "--skip-git-repo-check",
            "--ignore-rules",
            "--sandbox",
            "read-only",
            "--cd",
        ])
        .arg(&probe_dir)
        .arg("Reply with exactly CONTEXT_GUARDIAN_PROBE_OK. Do not call tools.")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_dir_all(&probe_dir);
            return LiveProbeResult {
                attempted: true,
                succeeded: false,
                status: format!("probe_spawn_failed:{:?}", error.kind()),
            };
        }
    };

    let started = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                break LiveProbeResult {
                    attempted: true,
                    succeeded: true,
                    status: "succeeded".to_string(),
                }
            }
            Ok(Some(status)) => {
                break LiveProbeResult {
                    attempted: true,
                    succeeded: false,
                    status: format!("probe_exit:{}", status.code().unwrap_or(-1)),
                }
            }
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break LiveProbeResult {
                    attempted: true,
                    succeeded: false,
                    status: "timed_out".to_string(),
                };
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break LiveProbeResult {
                    attempted: true,
                    succeeded: false,
                    status: format!("probe_wait_failed:{:?}", error.kind()),
                };
            }
        }
    };
    let _ = fs::remove_dir_all(&probe_dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_current_compacted_message_shape() {
        let value = json!({
            "type": "compacted",
            "payload": {
                "message": "summary",
                "replacement_history": [{
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "hello"}]
                }]
            }
        });
        let diagnosis = diagnose_records([&value]);
        assert!(diagnosis.issues.is_empty());
        assert!(diagnosis.repaired_values.is_empty());
    }

    #[test]
    fn repairs_stringified_compacted_history_and_content() {
        let value = json!({
            "type": "compacted",
            "payload": {
                "message": [{"text": "summary"}],
                "replacement_history": "[{\"role\":\"user\",\"content\":\"hello\"}]"
            }
        });
        let diagnosis = diagnose_records([&value]);
        assert_eq!(diagnosis.repaired_records(), 1);
        assert_eq!(diagnosis.unrepaired_issues(), 0);
        let repaired = &diagnosis.repaired_values[&0];
        assert_eq!(repaired["payload"]["message"], "summary");
        assert_eq!(
            repaired["payload"]["replacement_history"][0]["type"],
            "message"
        );
        assert_eq!(
            repaired["payload"]["replacement_history"][0]["content"][0],
            json!({"type": "input_text", "text": "hello"})
        );
    }

    #[test]
    fn repairs_role_mismatched_text_types() {
        let value = json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "input_text", "content": "answer"}]
            }
        });
        let diagnosis = diagnose_records([&value]);
        let repaired = &diagnosis.repaired_values[&0];
        assert_eq!(repaired["payload"]["content"][0]["type"], "output_text");
        assert_eq!(repaired["payload"]["content"][0]["text"], "answer");
        assert!(repaired["payload"]["content"][0].get("content").is_none());
    }

    #[test]
    fn serializes_structured_function_arguments() {
        let value = json!({
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "tool",
                "call_id": "call_1",
                "arguments": {"path": "/tmp/a"}
            }
        });
        let diagnosis = diagnose_records([&value]);
        assert_eq!(
            diagnosis.repaired_values[&0]["payload"]["arguments"],
            "{\"path\":\"/tmp/a\"}"
        );
    }

    #[test]
    fn reports_unrepairable_missing_message_role() {
        let value = json!({
            "type": "compacted",
            "payload": {
                "message": "summary",
                "replacement_history": [{
                    "type": "message",
                    "content": [{"type": "input_text", "text": "hello"}]
                }]
            }
        });
        let diagnosis = diagnose_records([&value]);
        assert_eq!(diagnosis.unrepaired_issues(), 1);
        assert!(diagnosis.repaired_values.is_empty());
    }

    #[test]
    fn detects_task_errors_without_reading_error_text() {
        let value = json!({
            "type": "event_msg",
            "payload": {"type": "task_complete", "error": {"message": "unknown"}}
        });
        assert!(is_task_error(&value));
    }

    #[cfg(unix)]
    #[test]
    fn live_probe_uses_exit_status_without_capturing_output() {
        let result = run_live_probe(Path::new("/usr/bin/true"), Duration::from_secs(5));
        assert!(result.attempted);
        assert!(result.succeeded);
        assert_eq!(result.status, "succeeded");
    }
}
