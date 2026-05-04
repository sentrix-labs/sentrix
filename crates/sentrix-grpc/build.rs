// build.rs — compile proto/sentrix.proto into Rust types via tonic-build.
//
// Generated module is included via `tonic::include_proto!("sentrix.v1")` in
// src/lib.rs. Re-run cargo build whenever proto/sentrix.proto changes.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["proto/sentrix.proto"], &["proto"])?;
    Ok(())
}
