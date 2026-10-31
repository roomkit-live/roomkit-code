//! Embed the compilation target and explicit distribution build marker.
fn main() {
    println!("cargo:rerun-if-env-changed=ROOMKIT_RELEASE_BUILD");
    println!(
        "cargo:rustc-env=ROOMKIT_BUILD_TARGET={}",
        std::env::var("TARGET").unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=ROOMKIT_RELEASE_BUILD={}",
        std::env::var("ROOMKIT_RELEASE_BUILD").unwrap_or_else(|_| "0".into())
    );
}
