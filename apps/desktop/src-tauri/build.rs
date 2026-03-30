fn main() {
    stage_native_helpers();

    #[cfg(target_os = "macos")]
    {
        let manifest_dir =
            std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
        let source = manifest_dir.join("native").join("xpc_bridge.m");
        let header_dir = manifest_dir
            .parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::parent)
            .expect("workspace root")
            .join("docs")
            .join("ipc");

        println!("cargo:rerun-if-changed={}", source.display());
        println!(
            "cargo:rerun-if-changed={}",
            header_dir.join("xpc_bridge.h").display()
        );
        println!("cargo:rustc-link-lib=framework=Foundation");

        cc::Build::new()
            .file(source)
            .include(header_dir)
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .compile("cloneaprocess_xpc_bridge");
    }

    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["system_status"])),
    )
    .expect("failed to run tauri-build");
}

fn stage_native_helpers() {
    let manifest_dir =
        std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let resources_dir = manifest_dir.join("resources").join("macos");

    std::fs::create_dir_all(&resources_dir).expect("create resources/macos");

    stage_native_helper(
        workspace_root
            .join("native")
            .join("mac-recorder-service")
            .join(".build")
            .join("debug")
            .join("RecorderService"),
        resources_dir.join("RecorderService"),
    );
    stage_native_helper(
        workspace_root
            .join("native")
            .join("mac-runner-service")
            .join(".build")
            .join("debug")
            .join("RunnerService"),
        resources_dir.join("RunnerService"),
    );
}

fn stage_native_helper(source: std::path::PathBuf, destination: std::path::PathBuf) {
    println!("cargo:rerun-if-changed={}", source.display());
    if !source.exists() {
        return;
    }

    std::fs::copy(&source, &destination).unwrap_or_else(|error| {
        panic!(
            "failed to stage native helper from {} to {}: {}",
            source.display(),
            destination.display(),
            error
        )
    });

    let permissions = std::fs::metadata(&source)
        .unwrap_or_else(|error| {
            panic!(
                "failed to read helper metadata for {}: {}",
                source.display(),
                error
            )
        })
        .permissions();
    std::fs::set_permissions(&destination, permissions).unwrap_or_else(|error| {
        panic!(
            "failed to set helper permissions on {}: {}",
            destination.display(),
            error
        )
    });
}
