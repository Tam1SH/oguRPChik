fn main() {
    let mut cmd = capnpc::CompilerCommand::new();
    cmd.src_prefix("schema").file("schema/ping.capnp");

    // Windows winget install doesn't put the stdlib .capnp files (stream.capnp
    // etc.) on a path capnpc discovers automatically; point at them explicitly.
    let std_import = std::path::Path::new(
        "C:/Users/ignat/AppData/Local/Microsoft/WinGet/Packages/\
         capnproto.capnproto_Microsoft.Winget.Source_8wekyb3d8bbwe/capnproto-c++-1.1.0/src",
    );
    if std_import.exists() {
        cmd.import_path(std_import);
    }

    cmd.run().expect("capnp schema compilation failed");
}
