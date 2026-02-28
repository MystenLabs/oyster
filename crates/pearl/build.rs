//! Build script — compile the Pearl protobuf definitions.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed=proto/pearl.proto");
    tonic_prost_build::compile_protos("proto/pearl.proto")?;
    Ok(())
}
