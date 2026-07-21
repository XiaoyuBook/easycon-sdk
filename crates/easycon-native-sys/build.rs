use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

fn main() {
    if env::var_os("CARGO_CFG_TARGET_OS").as_deref() != Some(std::ffi::OsStr::new("windows"))
        || env::var_os("CARGO_CFG_TARGET_ARCH").as_deref() != Some(std::ffi::OsStr::new("x86_64"))
    {
        panic!("easycon-native-sys supports Windows x64 only");
    }

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

    let configure_args = vec![
        OsString::from("-S"),
        repository.as_os_str().to_owned(),
        OsString::from("-B"),
        build_dir.as_os_str().to_owned(),
        OsString::from("-G"),
        OsString::from("Ninja"),
        OsString::from("-DCMAKE_BUILD_TYPE=Release"),
        OsString::from("-DCMAKE_CXX_COMPILER=cl.exe"),
        OsString::from("-DBUILD_TESTING=OFF"),
        OsString::from("-DEASYCON_BUILD_COMPONENT_TESTS=OFF"),
        OsString::from("-DEASYCON_BUILD_FUZZERS=OFF"),
        OsString::from("-DEASYCON_NATIVE_RUST_BUILD=ON"),
        prefixed_path("-DCMAKE_TOOLCHAIN_FILE=", &toolchain),
        OsString::from("-DVCPKG_TARGET_TRIPLET=x64-windows-static-md"),
        prefixed_path(
            "-DVCPKG_OVERLAY_TRIPLETS=",
            &repository.join("cmake/triplets"),
        ),
    ];
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

    println!(
        "cargo:rustc-link-search=native={}",
        build_dir.join("lib").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        build_dir
            .join("vcpkg_installed/x64-windows-static-md/lib")
            .display()
    );
    println!("cargo:rustc-link-lib=static=easycon_native_bridge");
    for library in [
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
        "advapi32", "comctl32", "comdlg32", "gdi32", "ole32", "oleaut32", "shell32", "user32",
        "uuid", "winspool",
    ] {
        println!("cargo:rustc-link-lib=dylib={library}");
    }

    for input in [
        "CMakeLists.txt",
        "vcpkg.json",
        "vcpkg-configuration.json",
        "cmake/triplets/x64-windows-static-md.cmake",
        "native/bridge/CMakeLists.txt",
        "native/bridge/include/internal/easycon_native_bridge.h",
        "native/bridge/src/bridge.cpp",
        "native/bridge/src/bridge_internal.hpp",
        "native/bridge/src/image_codec.cpp",
        "native/bridge/src/vision_ops.cpp",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            repository.join(input).display()
        );
    }
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
}

fn required_env(name: &str) -> OsString {
    env::var_os(name).unwrap_or_else(|| panic!("required environment variable {name} is not set"))
}

fn prefixed_path(prefix: &str, path: &Path) -> OsString {
    let mut value = OsString::from(prefix);
    value.push(path.as_os_str());
    value
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
