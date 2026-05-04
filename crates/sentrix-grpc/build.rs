// build.rs — compile proto/sentrix.proto into Rust types via tonic-build.
//
// Generated module is included via `tonic::include_proto!("sentrix.v1")` in
// src/lib.rs. Re-run cargo build whenever proto/sentrix.proto changes.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 2026-05-05: Ubuntu 22.04's apt-installed protoc (3.12.x) treats
    // proto3 `optional` fields as experimental and rejects without the
    // explicit flag. Modern protoc (≥ 3.15) accepts them by default.
    // Pass the flag so CI runners with the older apt package don't
    // exit 101 on the `optional BlockHeight at_height = 2` field in
    // GetBalanceRequest.
    let mut config = prost_build::Config::new();
    config.protoc_arg("--experimental_allow_proto3_optional");

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos_with_config(config, &["proto/sentrix.proto"], &["proto"])?;
    Ok(())
}
