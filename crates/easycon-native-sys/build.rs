use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

struct Target {
    os: String,
    triplet: &'static str,
    compiler: OsString,
    shared_bridge: bool,
}

fn main() {
    let target = target_configuration();
    let manifest_dir = PathBuf::from(required_env("CARGO_MANIFEST_DIR"));
    let repository = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("native-sys remains two levels below the repository root");
    let output_dir = PathBuf::from(required_env("OUT_DIR"));
    let build_dir = output_dir.join("cmake");
    let vcpkg_root = PathBuf::from(required_env("VCPKG_ROOT"));
    let toolchain = vcpkg_root.join("scripts/buildsystems/vcpkg.cmake");
    if !toolchain.is_file() {
        panic!("VCPKG_ROOT does not contain scripts/buildsystems/vcpkg.cmake");
    }

    let mut configure_args = vec![
        OsString::from("-S"),
        repository.as_os_str().to_owned(),
        OsString::from("-B"),
        build_dir.as_os_str().to_owned(),
        OsString::from("-G"),
        OsString::from("Ninja"),
        OsString::from("-DCMAKE_BUILD_TYPE=Release"),
        prefixed_value("-DCMAKE_CXX_COMPILER=", &target.compiler),
        OsString::from("-DBUILD_TESTING=OFF"),
        OsString::from("-DEASYCON_BUILD_COMPONENT_TESTS=OFF"),
        OsString::from("-DEASYCON_BUILD_FUZZERS=OFF"),
        OsString::from("-DEASYCON_NATIVE_RUST_BUILD=ON"),
        prefixed_path("-DCMAKE_TOOLCHAIN_FILE=", &toolchain),
        OsString::from(format!("-DVCPKG_TARGET_TRIPLET={}", target.triplet)),
    ];
    if target.os == "windows" {
        configure_args.push(prefixed_path(
            "-DVCPKG_OVERLAY_TRIPLETS=",
            &repository.join("cmake/triplets"),
        ));
    } else if target.os == "macos" {
        if env::var_os("EASYCON_EXPERIMENTAL_MACOS").as_deref() != Some(OsStr::new("1")) {
            panic!(
                "macOS is build-unverified; set EASYCON_EXPERIMENTAL_MACOS=1 for an explicit source-boundary build"
            );
        }
        configure_args.push(OsString::from("-DEASYCON_EXPERIMENTAL_MACOS=ON"));
        configure_args.push(OsString::from("-DCMAKE_OSX_ARCHITECTURES=arm64"));
    }
    run(
        "cmake",
        configure_args,
        repository,
        "configure native bridge",
    );
    run(
        "cmake",
        vec![
            OsString::from("--build"),
            build_dir.as_os_str().to_owned(),
            OsString::from("--target"),
            OsString::from("easycon_native_bridge"),
            OsString::from("--parallel"),
        ],
        repository,
        "build native bridge",
    );

    let library_dir = build_dir.join("lib");
    println!("cargo:rustc-link-search=native={}", library_dir.display());
    if target.shared_bridge {
        println!("cargo:rustc-link-lib=dylib=easycon_native_bridge");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", library_dir.display());
    } else {
        link_windows_static_bridge(&build_dir);
    }

    for input in [
        "CMakeLists.txt",
        "CMakePresets.json",
        "vcpkg.json",
        "vcpkg-configuration.json",
        "cmake/triplets/x64-windows-static-md.cmake",
        "native/bridge/CMakeLists.txt",
        "native/bridge/include/internal/easycon_native_bridge.h",
        "native/bridge/src/common/bridge.cpp",
        "native/bridge/src/common/bridge_internal.hpp",
        "native/bridge/src/common/capture_common.cpp",
        "native/bridge/src/common/capture_platform.hpp",
        "native/bridge/src/common/image_codec.cpp",
        "native/bridge/src/common/ocr.cpp",
        "native/bridge/src/common/vision_ops.cpp",
        "native/bridge/src/platform/windows/capture.cpp",
        "native/bridge/src/platform/linux/capture.cpp",
        "native/bridge/src/platform/macos/capture_unavailable.cpp",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            repository.join(input).display()
        );
    }
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-env-changed=CXX");
    println!("cargo:rerun-if-env-changed=EASYCON_EXPERIMENTAL_MACOS");
}

fn target_configuration() -> Target {
    let os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo target OS is set");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo target architecture is set");
    let compiler = env::var_os("CXX").unwrap_or_else(|| match os.as_str() {
        "windows" => OsString::from("cl.exe"),
        "linux" => OsString::from("c++"),
        "macos" => OsString::from("clang++"),
        _ => OsString::new(),
    });
    match (os.as_str(), arch.as_str()) {
        ("windows", "x86_64") => Target {
            os,
            triplet: "x64-windows-static-md",
            compiler,
            shared_bridge: false,
        },
        ("linux", "x86_64") => Target {
            os,
            triplet: "x64-linux",
            compiler,
            shared_bridge: true,
        },
        ("macos", "aarch64") => Target {
            os,
            triplet: "arm64-osx",
            compiler,
            shared_bridge: true,
        },
        _ => panic!(
            "easycon-native-sys supports Windows x64, Linux x64, and experimental macOS arm64; got {os}/{arch}"
        ),
    }
}

fn link_windows_static_bridge(build_dir: &Path) {
    println!(
        "cargo:rustc-link-search=native={}",
        build_dir
            .join("vcpkg_installed/x64-windows-static-md/lib")
            .display()
    );
    println!("cargo:rustc-link-lib=static=easycon_native_bridge");
    for library in [
        "tesseract55",
        "archive",
        "bz2",
        "lz4",
        "lzma",
        "zstd",
        "libcrypto",
        "libcurl",
        "leptonica-1.87.0",
        "gif",
        "openjp2",
        "tiff",
        "libwebpmux",
        "libwebp",
        "libsharpyuv",
        "opencv_videoio4",
        "opencv_imgcodecs4",
        "jpeg",
        "libpng16",
        "opencv_imgproc4",
        "opencv_core4",
        "zs",
    ] {
        println!("cargo:rustc-link-lib=static={library}");
    }
    println!("cargo:rustc-link-lib=dylib=msvcprt");
    for library in [
        "advapi32",
        "bcrypt",
        "comctl32",
        "comdlg32",
        "crypt32",
        "gdi32",
        "iphlpapi",
        "mf",
        "mfplat",
        "mfuuid",
        "ole32",
        "oleaut32",
        "secur32",
        "shell32",
        "shlwapi",
        "strmiids",
        "user32",
        "uuid",
        "windowscodecs",
        "winspool",
        "ws2_32",
        "xmllite",
    ] {
        println!("cargo:rustc-link-lib=dylib={library}");
    }
}

fn required_env(name: &str) -> OsString {
    env::var_os(name).unwrap_or_else(|| panic!("required environment variable {name} is not set"))
}

fn prefixed_path(prefix: &str, path: &Path) -> OsString {
    prefixed_value(prefix, path.as_os_str())
}

fn prefixed_value(prefix: &str, value: &OsStr) -> OsString {
    let mut output = OsString::from(prefix);
    output.push(value);
    output
}

fn run(program: &str, args: Vec<OsString>, current_dir: &Path, description: &str) {
    let status = Command::new(program)
        .args(&args)
        .current_dir(current_dir)
        .status()
        .unwrap_or_else(|error| panic!("failed to {description}: {error}"));
    ensure_success(status, description);
}

fn ensure_success(status: ExitStatus, description: &str) {
    assert!(status.success(), "failed to {description}: {status}");
}
