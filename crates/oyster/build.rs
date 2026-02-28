//! Build script — compile the Pearl protobuf definitions.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed=../pearl/proto/pearl.proto");
    tonic_prost_build::compile_protos("../pearl/proto/pearl.proto")?;
    Ok(())
}
