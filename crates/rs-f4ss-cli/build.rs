fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    // Capture git hash for version string
    let git_hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_HASH={git_hash}");

    // NOTE: WinFsp delayload is handled by winfsp-sys's build.rs:
    //   println!("cargo:rustc-link-lib=dylib=delayimp");
    //   println!("cargo:rustc-link-lib=dylib=winfsp-x64");      // arch-specific
    //   println!("cargo:rustc-link-arg=/DELAYLOAD:winfsp-x64.dll");
    // Do NOT add our own delayload directives here — they would conflict.
}
