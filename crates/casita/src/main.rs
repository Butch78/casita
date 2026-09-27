//! The `casita` command-line tool.

mod cli;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    cli::run().await
}
