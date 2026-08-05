fn main() {
    capnpc::CompilerCommand::new()
        .src_prefix("schema")
        .file("schema/echo.capnp")
        .run()
        .expect("capnp schema compilation failed; is the `capnp` binary installed?");

    println!("cargo:rerun-if-changed=schema/echo.capnp");
}
