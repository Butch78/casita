//! Development bundle setup using the same activation code as production.
fn main() -> std::io::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err(std::io::Error::other(
            "usage: fskit-native-setup APP_BUNDLE MODULE_IDENTIFIER",
        ));
    }
    let module = args[1]
        .to_str()
        .ok_or_else(|| std::io::Error::other("invalid module identifier"))?;
    #[cfg(unix)]
    return fskit_native::setup::register(
        std::path::Path::new(&args[0]),
        "casita-native-fskit-extension.appex",
        module,
    );
    #[cfg(not(unix))]
    Err(std::io::Error::other("FSKit setup requires macOS"))
}
