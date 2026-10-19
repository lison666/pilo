use std::{path::PathBuf, process::Command, time::UNIX_EPOCH};

use pilo_protocol::MAX_BINARY_PAYLOAD_BYTES;
use serde::Deserialize;
use serde_json::{Value, json};

use super::agent_dir;
use super::workspace_lock::guard_workspace_lock;
use crate::to_value;

#[derive(Deserialize)]
pub(crate) struct SessionReadParams {
    path: String,
    #[serde(default)]
    offset: u64,
    #[serde(default = "default_session_read_limit")]
    limit: usize,
}

#[derive(Deserialize)]
pub(crate) struct SessionDeleteParams {
    path: String,
}

const fn default_session_read_limit() -> usize {
    8 * 1024 * 1024
}

fn checked_session_file(path: &str) -> Result<PathBuf, String> {
    let requested = PathBuf::from(path);
    if requested.extension().and_then(|value| value.to_str()) != Some("jsonl") {
        return Err("session path must point to a JSONL file".to_owned());
    }
    let sessions_root = agent_dir()
        .map(|root| root.join("sessions"))
        .ok_or_else(|| "Pi agent directory is unavailable".to_owned())?
        .canonicalize()
        .map_err(|error| format!("Pi session directory is not accessible: {error}"))?;
    let canonical = requested
        .canonicalize()
        .map_err(|error| format!("session file '{path}' is not accessible: {error}"))?;
    if !canonical.starts_with(&sessions_root) {
        return Err("session path must stay inside the Pi session directory".to_owned());
    }
    Ok(canonical)
}

pub(crate) fn session_read(params: SessionReadParams) -> Result<(Value, Vec<u8>), String> {
    let path = checked_session_file(&params.path)?;
    let mut file = std::fs::File::open(&path)
        .map_err(|error| format!("failed to open session '{}': {error}", path.display()))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    let file_size = metadata.len();
    let file_mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos() as u64)
        .unwrap_or(0);
    use std::io::{Read as _, Seek as _, SeekFrom};
    file.seek(SeekFrom::Start(params.offset))
        .map_err(|error| error.to_string())?;
    let limit = params.limit.min(MAX_BINARY_PAYLOAD_BYTES);
    let snapshot_remaining = file_size.saturating_sub(params.offset);
    let read_limit = (limit as u64).min(snapshot_remaining);
    let mut data = Vec::with_capacity((read_limit as usize).min(1024 * 1024));
    file.take(read_limit)
        .read_to_end(&mut data)
        .map_err(|error| error.to_string())?;
    let next_offset = params.offset.saturating_add(data.len() as u64);
    Ok((
        json!({
            "nextOffset": next_offset,
            "eof": next_offset >= file_size,
            "fileSize": file_size,
            "fileMtimeNs": file_mtime_ns,
        }),
        data,
    ))
}

/// Delete a Pi session using the same policy as Pi's interactive session selector:
/// prefer the optional `trash` CLI, then fall back to permanent file removal.
pub(crate) fn session_delete(params: SessionDeleteParams) -> Result<Value, String> {
    let path = checked_session_file(&params.path)?;
    guard_workspace_lock(&path)?;
    let mut trash = Command::new("trash");
    if params.path.starts_with('-') {
        trash.arg("--");
    }
    let trash_status = trash.arg(&path).status();
    if trash_status.is_ok_and(|status| status.success()) || !path.exists() {
        return Ok(json!({ "method": "trash" }));
    }

    std::fs::remove_file(&path)
        .map_err(|error| format!("failed to delete session '{}': {error}", path.display()))?;
    Ok(json!({ "method": "unlink" }))
}

pub(crate) fn session_discover() -> Result<Value, String> {
    let Some(root) = agent_dir().map(|root| root.join("sessions")) else {
        return to_value(Vec::<Value>::new());
    };
    let mut headers = Vec::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return to_value(headers);
    };
    'outer: for project in projects.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for entry in files.flatten() {
            if headers.len() >= 500 {
                break 'outer;
            }
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(file) = std::fs::File::open(path) else {
                continue;
            };
            let mut reader = std::io::BufReader::new(file);
            let mut line = String::new();
            use std::io::BufRead as _;
            if reader.read_line(&mut line).is_ok()
                && let Ok(value) = serde_json::from_str::<Value>(line.trim())
            {
                headers.push(value);
            }
        }
    }
    to_value(headers)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::PathBuf;

    use super::{SessionDeleteParams, session_delete};
    use crate::session::agent_dir;

    /// Session file inside the real `<agent_dir>/sessions/` plus a workspace
    /// carrying the lock, so `checked_session_file`'s containment rule and the
    /// guard both see their true inputs.
    fn locked_session_fixture() -> Option<(PathBuf, PathBuf, PathBuf)> {
        let sessions = agent_dir()?.join("sessions");
        std::fs::create_dir_all(&sessions).ok()?;
        let tag = format!("pilo-guard-test-{}", std::process::id());
        let workspace = std::env::temp_dir().join(&tag);
        let project_dir = sessions.join(&tag);
        std::fs::create_dir_all(workspace.join("state")).ok()?;
        std::fs::create_dir_all(&project_dir).ok()?;
        let session = project_dir.join("session.jsonl");
        let mut file = std::fs::File::create(&session).ok()?;
        writeln!(
            file,
            "{{\"type\":\"session\",\"cwd\":{}}}",
            serde_json::to_string(&workspace.to_string_lossy()).unwrap()
        )
        .ok()?;
        Some((workspace, project_dir, session))
    }

    #[test]
    fn session_delete_refuses_a_live_lock_then_allows_once_released() {
        let Some((workspace, project_dir, session)) = locked_session_fixture() else {
            eprintln!("skipping: Pi agent directory unavailable");
            return;
        };
        std::fs::write(
            workspace.join("state/workspace.lock"),
            format!(
                "{{\"schema_version\":\"1.0\",\"pid\":{}}}",
                std::process::id()
            ),
        )
        .unwrap();

        let path = session.to_string_lossy().into_owned();
        let error = session_delete(SessionDeleteParams { path: path.clone() }).unwrap_err();
        assert!(error.contains("locked workspace"), "{error}");
        assert!(session.exists(), "locked transcript must survive delete");

        std::fs::remove_file(workspace.join("state/workspace.lock")).unwrap();
        session_delete(SessionDeleteParams { path }).unwrap();
        assert!(!session.exists(), "released transcript must be deleted");

        std::fs::remove_dir_all(&workspace).ok();
        std::fs::remove_dir_all(&project_dir).ok();
    }
}
