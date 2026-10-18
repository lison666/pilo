mod activity;
mod index;
mod io;
mod search;
mod watch;
mod workspace_lock;

use std::path::PathBuf;

pub(crate) use activity::session_activity;
pub(crate) use index::session_scan;
pub(crate) use io::{session_delete, session_discover, session_read};
pub(crate) use search::session_search;
pub(crate) use watch::{session_watch_start, session_watch_stop};

#[cfg(test)]
pub(crate) use index::{
    SESSION_INDEX_HEADER_BYTES, SESSION_INDEX_PREFIX_STAGES, SESSION_INDEX_TAIL_BYTES,
    SessionIndexSummary, scan_index, summarize_index_bytes,
};
#[cfg(test)]
pub(crate) use watch::{collect_session_change_paths, is_session_change};

fn session_dir_key(project: &str) -> String {
    let project = project.trim();
    // Pi removes one leading separator, including for UNC paths.
    let normalized = project
        .strip_prefix('/')
        .or_else(|| project.strip_prefix('\\'))
        .unwrap_or(project);
    format!("--{}--", normalized.replace(['/', '\\', ':'], "-"))
}

fn agent_dir() -> Option<PathBuf> {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".pi/agent")))
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".pi/agent"))
        })
}

#[cfg(test)]
mod tests {
    use super::session_dir_key;

    #[test]
    fn session_directory_keys_match_pi_for_drive_unc_and_posix_paths() {
        for (project, expected) in [
            (r"D:\work", "--D--work--"),
            ("D:/work", "--D--work--"),
            (r"\\host\share\work", "---host-share-work--"),
            ("//host/share/work", "---host-share-work--"),
            ("/home/user/work", "--home-user-work--"),
            (" /home/user/work ", "--home-user-work--"),
            ("/", "----"),
        ] {
            assert_eq!(session_dir_key(project), expected, "project={project:?}");
        }
    }
}
