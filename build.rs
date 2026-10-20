use std::path::Path;
use std::process::Command;

fn main() {
    build_vnc();

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

fn build_vnc() {
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-env-changed=CMAKE_PREFIX_PATH");
    println!("cargo:rerun-if-changed=vendor/tigervnc");

    let source = Path::new("vendor/tigervnc");

    assert!(
        source.join("CMakeLists.txt").exists(),
        "Run git submodule update --init --recursive first"
    );

    let prefix = std::env::var("CMAKE_PREFIX_PATH").unwrap_or_else(|_| "/opt/homebrew;/usr/local".into());
    let output = cmake::Config::new(source)
        .define("BUILD_VIEWER", "OFF")
        .define("ENABLE_NLS", "OFF")
        .define("ENABLE_H264", "OFF")
        .define("ENABLE_GNUTLS", "OFF")
        .define("ENABLE_NETTLE", "OFF")
        .define("CMAKE_PREFIX_PATH", prefix)
        .build_target("rfb")
        .build();
    let build = output.join("build");
    let cache = std::fs::read_to_string(build.join("CMakeCache.txt")).expect("TigerVNC CMake cache missing");
    let setting =
        |name: &str| cache.lines().find_map(|line| line.strip_prefix(name)).expect("TigerVNC dependency missing");
    let pixman_include = setting("Pixman_INCLUDE_DIR:PATH=");

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .define("HAVE_CONFIG_H", None)
        .define("__ASSERT_MACROS_DEFINE_VERSIONS_WITHOUT_UNDERSCORES", "0")
        .include(source.join("common"))
        .include(&build)
        .include(pixman_include)
        .file("native/vnc.cpp")
        .compile("vnc_bridge");

    for (directory, library) in [("rfb", "rfb"), ("network", "network"), ("rdr", "rdr"), ("core", "core")] {
        println!(
            "cargo:rustc-link-search=native={}",
            build.join("common").join(directory).display()
        );
        println!("cargo:rustc-link-lib=static={library}");
    }

    for name in ["Pixman_LIBRARY:FILEPATH=", "JPEG_LIBRARY_RELEASE:FILEPATH="] {
        let library = Path::new(setting(name));

        println!("cargo:rustc-link-search=native={}", library.parent().unwrap().display());
    }

    println!("cargo:rustc-link-lib=pixman-1");
    println!("cargo:rustc-link-lib=jpeg");
    println!("cargo:rustc-link-lib=z");
    println!("cargo:rustc-link-lib=framework=Carbon");
}
