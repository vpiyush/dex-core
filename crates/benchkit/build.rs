//! Bakes build provenance into the binary for the methodology header.
//!
//! Captured here, at build time, because run-time `rustc --version` reports
//! whatever toolchain rustup resolves *then* — not necessarily the compiler
//! that built this binary (e.g. after a `rustup update` between build and run).
//! Cargo gives build scripts the resolved `RUSTC` and the exact flags it passes
//! (`CARGO_ENCODED_RUSTFLAGS`, 0x1f-separated), so what we stamp is what built
//! the code. The flags apply workspace-wide, so benchkit's view matches the
//! bench crate it is linked into.

fn main() {
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let version = std::process::Command::new(&rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=BENCHKIT_RUSTC_VERSION={version}");

    let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS")
        .map(|f| f.split('\x1f').collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    println!("cargo:rustc-env=BENCHKIT_RUSTFLAGS={flags}");
}
