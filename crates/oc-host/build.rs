//! Build the native plugin fixtures shared by host and integration tests.
//!
//! The fixture is a single Rust file compiled with `rustc` into a real cdylib,
//! then arranged as a platform bundle. It has no dependencies and is never
//! linked into the product.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const VST2_CALLBACK_CAPACITY: usize = 1024;

const VARIANTS: [(&str, &str); 5] = [
    ("gain", "OpenCompose Gain Fixture"),
    ("instrument", "OpenCompose Tone Fixture"),
    ("broken", "OpenCompose Broken Fixture"),
    ("no_output", "OpenCompose No Output Fixture"),
    ("multi_out", "OpenCompose Multi Out Fixture"),
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=tests/fixtures/vst2_fixture.rs");
    println!("cargo:rerun-if-changed=tests/fixtures/lv2_fixture_plugin.rs");
    let capacity_source =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("vst2_callback_capacity.rs");
    std::fs::write(
        capacity_source,
        format!("pub(super) const CAPACITY: usize = {VST2_CALLBACK_CAPACITY};\n"),
    )
    .expect("write VST2 callback capacity");
    build_ffi_guard();
    build_au_guard();

    let target = env::var("TARGET").unwrap_or_default();
    let host = env::var("HOST").unwrap_or_default();
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let fixtures = out_dir.join("vst2-fixtures");
    let _ = std::fs::remove_dir_all(&fixtures);
    if target != host {
        println!("cargo:warning=VST2 fixtures are only built for the host target");
        return;
    }

    let source = Path::new("tests/fixtures/vst2_fixture.rs");
    let extension = if target.contains("apple") {
        "dylib"
    } else if target.contains("windows") {
        "dll"
    } else {
        "so"
    };
    std::fs::create_dir_all(&fixtures).expect("fixture directory");

    for (variant, name) in VARIANTS {
        let library = out_dir.join(format!("oc_vst2_fixture_{variant}.{extension}"));
        let crate_name = format!("oc_vst2_fixture_{variant}");
        let configuration = (variant != "gain").then(|| format!("{variant}_mode"));
        build_rust_fixture(RustFixture {
            crate_name: &crate_name,
            source,
            output: &library,
            configuration: configuration.as_deref(),
            optimization: 1,
        });
        install(&library, &fixtures, name, &target);
    }

    let lv2_library = out_dir.join(format!("oc_lv2_fixture.{extension}"));
    build_rust_fixture(RustFixture {
        crate_name: "oc_lv2_fixture",
        source: Path::new("tests/fixtures/lv2_fixture_plugin.rs"),
        output: &lv2_library,
        configuration: None,
        optimization: 3,
    });
    println!(
        "cargo:rustc-env=OC_LV2_FIXTURE_PATH={}",
        lv2_library.display()
    );
    println!("cargo:rustc-env=OC_VST2_FIXTURE_DIR={}", fixtures.display());
}

struct RustFixture<'a> {
    crate_name: &'a str,
    source: &'a Path,
    output: &'a Path,
    configuration: Option<&'a str>,
    optimization: u8,
}
fn build_rust_fixture(fixture: RustFixture<'_>) {
    let mut command = Command::new(env::var_os("RUSTC").expect("RUSTC"));
    command
        .arg("--crate-name")
        .arg(fixture.crate_name)
        .arg("--crate-type")
        .arg("cdylib")
        .arg("--edition")
        .arg("2021")
        .arg("-C")
        .arg(format!("opt-level={}", fixture.optimization))
        .arg("-o")
        .arg(fixture.output)
        .arg(fixture.source);
    if let Some(configuration) = fixture.configuration {
        command.arg("--cfg").arg(configuration);
    }
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to run rustc for {}: {error}", fixture.crate_name));
    assert!(
        output.status.success(),
        "failed to build {}:\n{}",
        fixture.crate_name,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Compile the C++ exception barrier used around plugin calls.
///
/// Rust cannot catch a C++ exception, so the barrier has to be C++. `cc`
/// selects the platform compiler (clang on macOS, the system C++ compiler on
/// Linux, MSVC or clang-cl on Windows) and links the C++ runtime.
fn build_ffi_guard() {
    let target = env::var("TARGET").unwrap_or_default();
    let host = env::var("HOST").unwrap_or_default();
    // Guard against missing source file when building from a partial checkout.
    let source = Path::new("src/ffi_guard.cc");
    if !source.exists() {
        println!("cargo:warning=src/ffi_guard.cc is missing; skipping the C++ exception barrier");
        return;
    }
    println!("cargo:rerun-if-changed=src/ffi_guard.cc");
    println!("cargo:rerun-if-changed=src/clap_guard.cc");
    println!("cargo:rerun-if-changed=src/vst3_guard.cc");
    println!("cargo:rerun-if-changed=src/vst3_guard_abi.h");
    println!("cargo:rerun-if-changed=src/native_fault.h");
    println!("cargo:rerun-if-changed=src/vst2_callback_thunks.cc");
    println!("cargo:rerun-if-changed=src/vst2_guard.cc");
    println!("cargo:rerun-if-changed=src/vst2_guard_abi.h");
    println!("cargo:rerun-if-env-changed=CXX");
    // A cross build has no target C++ toolchain unless the caller points at
    // one. The Rust side still type-checks; only a real link needs the shim.
    if target != host && env::var_os("CXX").is_none() {
        println!(
            "cargo:warning=cross-compiling without CXX; the C++ exception barrier is not linked"
        );
        return;
    }
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file(source)
        .file("src/clap_guard.cc")
        .file("src/vst3_guard.cc")
        .file("src/vst2_callback_thunks.cc")
        .file("src/vst2_guard.cc")
        .define(
            "OC_VST2_CALLBACK_CAPACITY",
            Some(VST2_CALLBACK_CAPACITY.to_string().as_str()),
        );
    if target.contains("msvc") {
        // Catch C++ exceptions thrown across the C ABI functions.
        build.flag("/EHsc");
        // `std::exception` lives in the C++ runtime, which MSVC does not add
        // automatically to a C library link.
        println!("cargo:rustc-link-lib=msvcprt");
    } else {
        build.flag_if_supported("-fPIC");
    }
    build.compile("oc_ffi_guard");
    if target == host {
        println!("cargo:rerun-if-changed=tests/fixtures/clap_guard_fixture.cc");
        let mut fixture = cc::Build::new();
        fixture
            .cpp(true)
            .std("c++17")
            .opt_level(2)
            .file("tests/fixtures/clap_guard_fixture.cc")
            .cargo_metadata(false);
        if target.contains("msvc") {
            fixture.flag("/EHsc");
        }
        fixture.compile("oc_clap_guard_fixture");
        println!("cargo:rerun-if-changed=tests/fixtures/vst3_guard_fixture.cc");
        let mut vst3_fixture = cc::Build::new();
        vst3_fixture
            .cpp(true)
            .std("c++17")
            .opt_level(2)
            .file("tests/fixtures/vst3_guard_fixture.cc")
            .cargo_metadata(false);
        if target.contains("msvc") {
            vst3_fixture.flag("/EHsc");
        }
        vst3_fixture.compile("oc_vst3_guard_fixture");
        build_clap_module_fixture(&target);
        build_vst2_guard_fixture(&target);
        // The unit-test extern block requests this archive; production never links it.
        println!(
            "cargo:rustc-link-search=native={}",
            env::var("OUT_DIR").expect("OUT_DIR")
        );
    }
}

fn install(library: &Path, fixtures: &Path, name: &str, target: &str) {
    if target.contains("apple") {
        let bundle = fixtures.join(format!("{name}.vst"));
        let executable = bundle.join("Contents/MacOS").join(name);
        std::fs::create_dir_all(executable.parent().expect("macos directory"))
            .expect("create bundle");
        std::fs::copy(library, &executable).expect("copy fixture binary");
    } else {
        let extension = if target.contains("windows") {
            "dll"
        } else {
            "so"
        };
        std::fs::copy(library, fixtures.join(format!("{name}.{extension}")))
            .expect("copy fixture library");
    }
}

/// Build one optimized fake DSO; host unit tests copy it into isolated roots.
fn build_clap_module_fixture(target: &str) {
    println!("cargo:rerun-if-changed=tests/fixtures/clap_module_fixture.cc");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let extension = if target.contains("apple") {
        "dylib"
    } else if target.contains("windows") {
        "dll"
    } else {
        "so"
    };
    let library = out_dir.join(format!("oc_clap_module_fixture.{extension}"));
    let compiler = cc::Build::new()
        .cpp(true)
        .std("c++17")
        .opt_level(2)
        .cargo_metadata(false)
        .get_compiler();
    let mut command = compiler.to_command();
    if compiler.is_like_msvc() {
        command
            .arg("/LD")
            .arg("/EHsc")
            .arg(format!(
                "/Fo{}",
                out_dir.join("oc_clap_module_fixture.obj").display()
            ))
            .arg("tests/fixtures/clap_module_fixture.cc")
            .arg("/link")
            .arg(format!("/OUT:{}", library.display()));
    } else {
        command
            .arg(if target.contains("apple") {
                "-dynamiclib"
            } else {
                "-shared"
            })
            .arg("-fPIC")
            .arg("tests/fixtures/clap_module_fixture.cc")
            .arg("-o")
            .arg(&library);
    }
    let output = command
        .output()
        .expect("run compiler for CLAP module fixture");
    assert!(
        output.status.success(),
        "failed to build CLAP module fixture:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    println!(
        "cargo:rustc-env=OC_CLAP_MODULE_FIXTURE_PATH={}",
        library.display()
    );
}

/// The AU boundary catches native language exceptions before Rust C frames.
fn build_au_guard() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    for source in [
        "src/au_guard.mm",
        "src/au_guard_abi.h",
        "src/native_fault.h",
    ] {
        println!("cargo:rerun-if-changed={source}");
    }
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .flag("-fexceptions")
        .flag("-fobjc-exceptions")
        .file("src/au_guard.mm")
        .compile("oc_au_guard");
    println!("cargo:rustc-link-lib=objc");
    if env::var("TARGET").ok() == env::var("HOST").ok() {
        println!("cargo:rerun-if-changed=tests/fixtures/au_guard_fixture.mm");
        cc::Build::new()
            .cpp(true)
            .std("c++17")
            .flag("-fexceptions")
            .flag("-fobjc-exceptions")
            .file("tests/fixtures/au_guard_fixture.mm")
            .cargo_metadata(false)
            .compile("oc_au_guard_fixture");
        println!(
            "cargo:rustc-link-search=native={}",
            env::var("OUT_DIR").expect("OUT_DIR")
        );
    }
}

/// Two images share one compiled fixture object, allowing image-fault isolation
/// without per-case copied/locked DLL artifacts or another Rust test binary.
fn build_vst2_guard_fixture(target: &str) {
    println!("cargo:rerun-if-changed=tests/fixtures/vst2_guard_fixture.cc");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let compiler = cc::Build::new()
        .cpp(true)
        .std("c++17")
        .opt_level(2)
        .cargo_metadata(false)
        .get_compiler();
    let object = out_dir.join(if compiler.is_like_msvc() {
        "oc_vst2_guard_fixture.obj"
    } else {
        "oc_vst2_guard_fixture.o"
    });
    let mut compile = compiler.to_command();
    if compiler.is_like_msvc() {
        compile
            .arg("/c")
            .arg("/EHsc")
            .arg(format!("/Fo{}", object.display()))
            .arg("tests/fixtures/vst2_guard_fixture.cc");
    } else {
        compile
            .arg("-c")
            .arg("-fPIC")
            .arg("-pthread")
            .arg("tests/fixtures/vst2_guard_fixture.cc")
            .arg("-o")
            .arg(&object);
    }
    let output = compile.output().expect("compile native VST2 guard fixture");
    assert!(
        output.status.success(),
        "native VST2 fixture compilation failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let extension = if target.contains("windows") {
        "dll"
    } else if target.contains("apple") {
        "dylib"
    } else {
        "so"
    };
    for (name, environment) in [
        ("primary", "OC_VST2_GUARD_FIXTURE_PATH"),
        ("secondary", "OC_VST2_GUARD_SECONDARY_PATH"),
    ] {
        let library = out_dir.join(format!("oc_vst2_guard_{name}.{extension}"));
        let mut link = compiler.to_command();
        if compiler.is_like_msvc() {
            link.arg("/LD")
                .arg("/EHsc")
                .arg(&object)
                .arg("/link")
                .arg(format!("/OUT:{}", library.display()));
        } else {
            link.arg(if target.contains("apple") {
                "-dynamiclib"
            } else {
                "-shared"
            })
            .arg("-pthread")
            .arg(&object)
            .arg("-o")
            .arg(&library);
        }
        let output = link.output().expect("link native VST2 guard fixture");
        assert!(
            output.status.success(),
            "native VST2 fixture link failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        println!("cargo:rustc-env={environment}={}", library.display());
    }
}
