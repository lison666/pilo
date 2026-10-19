//! Guard against deleting transcripts of actively running workspaces.
//!
//! The bidding-agent harness writes `<workspace>/state/workspace.lock` while a
//! job holds the workspace. Deleting that job's Pi transcript underneath it
//! corrupts the run, so destructive session operations consult the lock first.
//! Pilo only ever reads the lock file; lifecycle (create/reclaim/release) stays
//! with the harness. See `workspace-lock.ts` in pi-bidding-agent.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
struct WorkspaceLockRecord {
    // schema_version is tolerated but not required: every supported version
    // carries the same holder fields we consume.
    pid: Option<u32>,
    role: Option<String>,
    started_at: Option<String>,
}

/// Error when the session transcript at `session_path` belongs to a workspace
/// whose lock holder is still alive; Ok for missing/malformed/stale locks.
pub(crate) fn guard_workspace_lock(session_path: &Path) -> Result<(), String> {
    let Some(cwd) = session_header_cwd(session_path) else {
        return Ok(());
    };
    let lock_path = cwd.join("state").join("workspace.lock");
    let Ok(text) = std::fs::read_to_string(&lock_path) else {
        return Ok(());
    };
    let Ok(lock) = serde_json::from_str::<WorkspaceLockRecord>(text.trim()) else {
        return Ok(());
    };
    let Some(pid) = lock.pid else {
        return Ok(());
    };
    if pid_alive(pid) {
        return Err(format!(
            "refusing to modify session of locked workspace '{}' ({} pid {pid} alive since {})",
            cwd.display(),
            lock.role.unwrap_or_else(|| "lock holder".to_owned()),
            lock.started_at.unwrap_or_else(|| "unknown time".to_owned()),
        ));
    }
    // Stale lock (dead pid): allow the operation; the harness owns cleanup.
    Ok(())
}

/// The transcript header's `cwd` is the workspace the session belongs to.
fn session_header_cwd(session_path: &Path) -> Option<PathBuf> {
    use std::io::BufRead as _;
    let file = std::fs::File::open(session_path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file).read_line(&mut line).ok()?;
    let header: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let cwd = PathBuf::from(header.get("cwd")?.as_str()?);
    // A header pointing at a non-directory cannot carry a lock file anyway.
    cwd.is_dir().then_some(cwd)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0): success or EPERM means the process exists; ESRCH means dead.
    unsafe {
        if libc::kill(pid as i32, 0) == 0 {
            return true;
        }
        matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        )
    }
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        // HANDLE is an isize in windows-sys; OpenProcess returns 0 on failure.
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle != 0 {
            CloseHandle(handle);
            return true;
        }
        // Access denied still proves the process exists.
        GetLastError() == ERROR_ACCESS_DENIED
    }
}

#[cfg(not(any(unix, windows)))]
fn pid_alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pilo-lock-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("state")).unwrap();
        dir
    }

    fn write_session(dir: &Path, cwd: &Path) -> PathBuf {
        let path = dir.join("session.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            "{{\"type\":\"session\",\"cwd\":{}}}",
            serde_json::to_string(&cwd.to_string_lossy()).unwrap()
        )
        .unwrap();
        path
    }

    #[test]
    fn alive_holder_blocks_the_operation() {
        let workspace = tempdir("alive-ws");
        let session_dir = tempdir("alive-sess");
        let session = write_session(&session_dir, &workspace);
        std::fs::write(
            workspace.join("state/workspace.lock"),
            format!(
                "{{\"schema_version\":\"1.0\",\"pid\":{},\"role\":\"run\",\"started_at\":\"2026-01-01T00:00:00Z\",\"host\":\"h\",\"workspace\":\"w\"}}",
                std::process::id()
            ),
        )
        .unwrap();
        let error = guard_workspace_lock(&session).unwrap_err();
        assert!(error.contains("pid"), "{error}");
        std::fs::remove_dir_all(&workspace).ok();
        std::fs::remove_dir_all(&session_dir).ok();
    }

    #[test]
    fn missing_malformed_and_stale_locks_allow_the_operation() {
        let cases = [
            ("", "missing file"),
            ("not json", "malformed"),
            ("{\"pid\":null}", "no pid"),
            // pid 2^30 is far outside any live range on unix and windows.
            ("{\"pid\":1073741824}", "stale dead pid"),
        ];
        for (lock_body, tag) in cases {
            let workspace = tempdir(&format!("ok-{tag}"));
            let session_dir = tempdir(&format!("ok-s-{tag}"));
            let session = write_session(&session_dir, &workspace);
            if !lock_body.is_empty() {
                std::fs::write(workspace.join("state/workspace.lock"), lock_body).unwrap();
            }
            assert!(guard_workspace_lock(&session).is_ok(), "case {tag}");
            std::fs::remove_dir_all(&workspace).ok();
            std::fs::remove_dir_all(&session_dir).ok();
        }
    }

    #[test]
    fn missing_header_or_cwd_allows_the_operation() {
        let dir = tempdir("nohdr");
        let session = dir.join("session.jsonl");
        std::fs::write(&session, "{\"type\":\"message\"}\n").unwrap();
        assert!(guard_workspace_lock(&session).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}
