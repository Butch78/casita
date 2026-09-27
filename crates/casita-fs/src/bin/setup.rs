//! Register and activate the packaged native Rust FSKit extension.
fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 || args[0] == "--help" {
        eprintln!("usage: casita-fs-setup CASITA_FSKIT_APP");
        return if args.len() == 1 && args[0] == "--help" {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::from(2)
        };
    }
    #[cfg(target_os = "macos")]
    let result = casita_fs::darwin::setup::register_native(std::path::Path::new(&args[0]));
    #[cfg(not(target_os = "macos"))]
    let result: std::io::Result<()> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "native FSKit setup requires macOS",
    ));
    match result {
        Ok(()) => {
            println!("Native Casita FSKit registered; macOS checks approval when mounting");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Native FSKit setup failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
