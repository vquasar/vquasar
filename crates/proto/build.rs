//! Compile `proto/agent.proto` into Rust gRPC server + client code.
//!
//! Uses a vendored `protoc` so the build is hermetic — no system protobuf
//! compiler is required on developer machines or CI.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    // Safety: single-threaded build script, set before tonic-build reads it.
    std::env::set_var("PROTOC", protoc);

    let proto_root = PathBuf::from("../../proto");
    let proto_file = proto_root.join("agent.proto");
    println!("cargo:rerun-if-changed={}", proto_file.display());

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&[proto_file], &[proto_root])?;
    Ok(())
}
