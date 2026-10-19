use std::path::Path;
use std::process::Command;

fn main() {
    for path in [
        "frontend/index.html",
        "frontend/package.json",
        "frontend/pnpm-lock.yaml",
        "frontend/tsconfig.json",
        "frontend/vite.config.ts",
        "frontend/src",
        "shared/stun-servers.json",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }

    let frontend = Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend");
    let mut install = Command::new("pnpm");

    install.arg("install").arg("--frozen-lockfile").current_dir(&frontend);

    if std::env::var_os("CARGO_NET_OFFLINE").is_some() {
        install.arg("--offline");
    }

    assert!(
        install.status().expect("pnpm is required to build the frontend").success(),
        "pnpm install failed"
    );
    assert!(
        Command::new("pnpm")
            .args(["run", "build"])
            .current_dir(frontend)
            .status()
            .expect("pnpm is required to build the frontend")
            .success(),
        "frontend build failed"
    );
}
