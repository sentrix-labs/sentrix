// build.rs — compile proto/sentrix.proto into Rust types via tonic-build.
//
// Generated module is included via `tonic::include_proto!("sentrix.v1")` in
// src/lib.rs. Re-run cargo build whenever proto/sentrix.proto changes.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Ubuntu 22.04's apt-installed protoc (3.12.x) treats proto3 `optional`
    // fields as experimental and rejects without the explicit flag. Modern
    // protoc (≥ 3.15) accepts them by default. Pass the flag so CI runners
    // with the older apt package don't exit 101 on the
    // `optional BlockHeight at_height = 2` field in GetBalanceRequest.
    let mut config = prost_build::Config::new();
    config.protoc_arg("--experimental_allow_proto3_optional");

    // The `transport` cargo feature gates whether the generated code may
    // reference `tonic::transport::*` (Channel / Server). WASM consumers
    // disable the feature so the generated module compiles without
    // tokio-net / mio dependencies. tonic-build emits transport-using
    // code only when both client/server stubs AND transport bindings are
    // requested.
    let with_transport = std::env::var_os("CARGO_FEATURE_TRANSPORT").is_some();

    tonic_prost_build::configure()
        .build_client(true)
        // Server stubs require the hyper-based transport stack (Server::builder),
        // so they're gated behind the same feature.
        .build_server(with_transport)
        .build_transport(with_transport)
        .compile_with_config(config, &["proto/sentrix.proto"], &["proto"])?;

    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_TRANSPORT");
    Ok(())
}
