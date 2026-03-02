//! Build script for xgboost-ffi: locates libxgboost static libraries
//! built by xgboost-sys and emits link directives.
//!
//! Searches target/{debug,release}/build/xgboost-sys-*/out/xgboost/ for the
//! pre-built static libraries. On ARM64 macOS, patches the dmlc-core Makefile
//! to remove -msse2 and rebuilds if libdmlc.a is missing.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Find the workspace target directory
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let out_path = PathBuf::from(&out_dir);

    // Walk up from OUT_DIR to find the target/ dir
    // OUT_DIR is like: .../target/debug/build/xgboost-ffi-HASH/out
    let target_root = out_path
        .ancestors()
        .find(|p| {
            p.file_name()
                .map(|f| f == "target")
                .unwrap_or(false)
        })
        .expect("Could not find target/ directory");

    // Search across both debug and release build directories
    let mut xgboost_root: Option<PathBuf> = None;
    for profile in &["debug", "release"] {
        let pattern = format!(
            "{}/{}/build/xgboost-sys-*/out/xgboost",
            target_root.display(),
            profile
        );
        for entry in glob::glob(&pattern).expect("glob pattern error") {
            if let Ok(path) = entry {
                if path.join("lib/libxgboost.a").exists() {
                    xgboost_root = Some(path);
                    break;
                }
            }
        }
        if xgboost_root.is_some() {
            break;
        }
    }

    let xgboost_root = match xgboost_root {
        Some(p) => p,
        None => {
            panic!(
                "Could not find pre-built xgboost static libraries in {:?}. \
                 Build the xgboost crate first (debug or release).",
                target_root
            );
        }
    };

    // Check if dmlc-core needs to be rebuilt (ARM64 -msse2 fix)
    let dmlc_lib = xgboost_root.join("dmlc-core/libdmlc.a");
    if !dmlc_lib.exists() {
        let dmlc_makefile = xgboost_root.join("dmlc-core/Makefile");
        if dmlc_makefile.exists() {
            patch_msse2(&dmlc_makefile);
            rebuild_dmlc(&xgboost_root);
        }
    }

    // Emit link search paths
    println!(
        "cargo:rustc-link-search=native={}",
        xgboost_root.join("lib").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        xgboost_root.join("rabit/lib").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        xgboost_root.join("dmlc-core").display()
    );

    // Emit link libraries
    println!("cargo:rustc-link-lib=static=xgboost");
    println!("cargo:rustc-link-lib=static=dmlc");

    // On Linux with OpenMP, xgboost builds librabit.a; otherwise librabit_empty.a
    if xgboost_root.join("rabit/lib/librabit.a").exists() {
        println!("cargo:rustc-link-lib=static=rabit");
        println!("cargo:rustc-link-lib=dylib=gomp");
    } else {
        println!("cargo:rustc-link-lib=static=rabit_empty");
    }

    // Link C++ runtime
    let target = env::var("TARGET").unwrap_or_default();
    if target.contains("apple") {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
}

fn patch_msse2(makefile: &Path) {
    if let Ok(content) = std::fs::read_to_string(makefile) {
        if content.contains("-msse2") {
            let patched = content.replace("-msse2", "");
            let _ = std::fs::write(makefile, patched);
        }
    }
}

fn rebuild_dmlc(xgboost_root: &Path) {
    let dmlc_dir = xgboost_root.join("dmlc-core");
    let config = xgboost_root.join("make/minimum.mk");
    let config_arg = format!("config={}", config.display());

    let _ = Command::new("make")
        .current_dir(&dmlc_dir)
        .args(["clean"])
        .output();

    let _ = Command::new("make")
        .current_dir(&dmlc_dir)
        .args(["libdmlc.a", &config_arg])
        .output();
}
