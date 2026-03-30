use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeHelper {
    Recorder,
    #[allow(dead_code)]
    Runner,
}

impl NativeHelper {
    fn binary_name(self) -> &'static str {
        match self {
            Self::Recorder => "RecorderService",
            Self::Runner => "RunnerService",
        }
    }

    fn env_var(self) -> &'static str {
        match self {
            Self::Recorder => "CLONEAPROCESS_RECORDER_BINARY",
            Self::Runner => "CLONEAPROCESS_RUNNER_BINARY",
        }
    }

    fn bundled_resource_path(self) -> &'static str {
        match self {
            Self::Recorder => "helpers/macos/RecorderService",
            Self::Runner => "helpers/macos/RunnerService",
        }
    }

    fn staged_repo_path(self) -> PathBuf {
        PathBuf::from("apps")
            .join("desktop")
            .join("src-tauri")
            .join("resources")
            .join("macos")
            .join(self.binary_name())
    }

    fn repo_build_path(self) -> PathBuf {
        let service_dir = match self {
            Self::Recorder => "mac-recorder-service",
            Self::Runner => "mac-runner-service",
        };

        PathBuf::from("native")
            .join(service_dir)
            .join(".build")
            .join("debug")
            .join(self.binary_name())
    }
}

pub fn resolve_helper_binary(app: &AppHandle, helper: NativeHelper) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| manifest_dir.clone());
    let fallback = workspace_root.join(helper.repo_build_path());

    let mut candidates = Vec::new();
    if let Some(path) = env_override_path(helper) {
        candidates.push(path);
    }

    candidates.push(workspace_root.join(helper.repo_build_path()));
    candidates.push(workspace_root.join(helper.staged_repo_path()));
    candidates.push(
        manifest_dir
            .join("resources")
            .join("macos")
            .join(helper.binary_name()),
    );

    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir.join(helper.repo_build_path()));
        candidates.push(current_dir.join(helper.staged_repo_path()));
    }

    if let Ok(resource_path) = app.path().resolve(
        helper.bundled_resource_path(),
        tauri::path::BaseDirectory::Resource,
    ) {
        candidates.push(resource_path);
    }

    first_existing_candidate(candidates).unwrap_or(fallback)
}

fn env_override_path(helper: NativeHelper) -> Option<PathBuf> {
    std::env::var(helper.env_var())
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn first_existing_candidate(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|path| path.exists())
}

#[cfg(test)]
mod tests {
    use super::{first_existing_candidate, NativeHelper};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn first_existing_candidate_prefers_first_match() {
        let temp_root =
            std::env::temp_dir().join(format!("cloneaprocess-helper-test-{}", std::process::id()));
        let first = temp_root.join("first");
        let second = temp_root.join("second");
        fs::create_dir_all(&temp_root).expect("create temp root");
        fs::write(&second, b"runner").expect("write second file");
        fs::write(&first, b"recorder").expect("write first file");

        let resolved = first_existing_candidate(vec![first.clone(), second.clone()]);
        assert_eq!(resolved, Some(first));

        let _ = fs::remove_file(second);
        let _ = fs::remove_file(temp_root.join("first"));
        let _ = fs::remove_dir(&temp_root);
    }

    #[test]
    fn helper_metadata_matches_expected_bundle_layout() {
        assert_eq!(
            NativeHelper::Recorder.staged_repo_path(),
            PathBuf::from("apps/desktop/src-tauri/resources/macos/RecorderService")
        );
        assert_eq!(
            NativeHelper::Runner.staged_repo_path(),
            PathBuf::from("apps/desktop/src-tauri/resources/macos/RunnerService")
        );
    }
}
