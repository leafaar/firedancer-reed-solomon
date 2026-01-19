use std::env;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let firedancer_dir = manifest_dir.join("firedancer");
    let reedsol_dir = firedancer_dir.join("src/ballet/reedsol");
    let wrapper_c = manifest_dir.join("src/wrapper.c");

    // Detect CPU features - check both target features and try to detect from the build host
    let target_features = env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();

    // For native builds, we can also enable features based on what the host supports
    // Check for RUSTFLAGS containing target-cpu=native or target-feature=+avx2
    let rustflags = env::var("RUSTFLAGS").unwrap_or_default();
    let use_native = rustflags.contains("target-cpu=native");

    // Determine available features
    let has_avx2 = target_features.contains("avx2")
        || use_native
        || std::is_x86_feature_detected!("avx2");
    let has_gfni = target_features.contains("gfni")
        || (use_native && std::is_x86_feature_detected!("gfni"));
    let has_avx512f = target_features.contains("avx512f")
        || (use_native && std::is_x86_feature_detected!("avx512f"));

    // Determine the arithmetic implementation to use
    // 0 = generic, 1 = AVX2, 2 = GFNI+AVX2, 3 = GFNI+AVX512
    let arith_impl = if has_gfni && has_avx512f {
        3
    } else if has_gfni && has_avx2 {
        2
    } else if has_avx2 {
        1
    } else {
        0
    };

    eprintln!(
        "firedancer-reed-solomon: Building with ARITH_IMPL={} (avx2={}, gfni={}, avx512={})",
        arith_impl, has_avx2, has_gfni, has_avx512f
    );

    println!("cargo:rerun-if-changed=src/wrapper.c");
    println!("cargo:rerun-if-changed=firedancer/src/ballet/reedsol/");

    // Build the C library
    let mut build = cc::Build::new();

    let wrapped_impl_dir = reedsol_dir.join("wrapped_impl");

    build
        .file(&wrapper_c)
        .file(reedsol_dir.join("fd_reedsol.c"))
        .file(reedsol_dir.join("fd_reedsol_pi.c"))
        .file(reedsol_dir.join("fd_reedsol_encode_16.c"))
        .file(reedsol_dir.join("fd_reedsol_encode_32.c"))
        .file(reedsol_dir.join("fd_reedsol_encode_64.c"))
        .file(reedsol_dir.join("fd_reedsol_encode_128.c"))
        .file(reedsol_dir.join("fd_reedsol_recover_16.c"))
        .file(reedsol_dir.join("fd_reedsol_recover_32.c"))
        .file(reedsol_dir.join("fd_reedsol_recover_64.c"))
        .file(reedsol_dir.join("fd_reedsol_recover_128.c"))
        .file(reedsol_dir.join("fd_reedsol_recover_256.c"))
        // Include wrapped implementations for larger FFT/PPT functions
        .file(wrapped_impl_dir.join("fd_reedsol_ppt_impl_17.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_ppt_impl_25.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_ppt_impl_33.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_ppt_impl_40.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_ppt_impl_45.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_ppt_impl_50.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_ppt_impl_55.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_ppt_impl_60.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_ppt_impl_65.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_fft_impl_64_0.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_fft_impl_64_64.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_fft_impl_64_128.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_fft_impl_128_0.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_fft_impl_128_128.c"))
        .file(wrapped_impl_dir.join("fd_reedsol_fft_impl_256_0.c"))
        .include(firedancer_dir.join("src"))
        // Define the Firedancer platform capabilities
        .define("FD_HAS_HOSTED", "1")
        .define("FD_HAS_ATOMIC", "1")
        .define("FD_HAS_THREADS", "1")
        .define("FD_HAS_INT128", "1")
        .define("FD_HAS_DOUBLE", "1")
        .define("FD_HAS_ALLOCA", "1")
        .define("FD_HAS_X86", "1")
        .std("c17")
        .opt_level(3)
        .warnings(false);

    // Set architecture-specific flags
    if has_avx512f {
        build
            .define("FD_HAS_SSE", "1")
            .define("FD_HAS_AVX", "1")
            .define("FD_HAS_AVX512", "1")
            .flag("-mavx512f")
            .flag("-mavx512bw")
            .flag("-mavx512dq")
            .flag("-mavx512vl");
    } else if has_avx2 {
        build
            .define("FD_HAS_SSE", "1")
            .define("FD_HAS_AVX", "1")
            .flag("-mavx2");
    }

    if has_gfni {
        build.define("FD_HAS_GFNI", "1").flag("-mgfni");
    }

    // Set the arithmetic implementation
    let arith_impl_str = arith_impl.to_string();
    build.define("FD_REEDSOL_ARITH_IMPL", arith_impl_str.as_str());

    // Include GFNI 32x32 encode if available
    if has_gfni {
        build.file(reedsol_dir.join("fd_reedsol_gfni_32.S"));
    }

    // Set the working directory for FD_IMPORT_BINARY paths
    // The paths in fd_reedsol.c are relative to the firedancer root
    let current_dir = env::current_dir().unwrap();
    env::set_current_dir(&firedancer_dir).expect("Failed to change to firedancer directory");

    build.compile("fd_reedsol");

    env::set_current_dir(&current_dir).expect("Failed to restore directory");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=fd_reedsol");
}
