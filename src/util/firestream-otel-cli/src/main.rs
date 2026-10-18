use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match otel_cli::run().await {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("otel-cli: {err:#}");
            ExitCode::from(1)
        }
    }
}
