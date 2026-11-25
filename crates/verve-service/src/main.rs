//! Minimal privileged IPC supervisor daemon for Verve.

mod allowlist;
mod ipc;
mod os_service;
mod supervisor;

use tracing_subscriber::EnvFilter;

fn main() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let cmd = match os_service::parse_args(std::env::args_os()) {
        Ok(cmd) => cmd,
        Err(err) => {
            eprintln!("error: {err}");
            os_service::print_usage();
            std::process::exit(2);
        }
    };

    if let Err(err) = os_service::execute_command(cmd) {
        tracing::error!(error = %err, "verve-service failed");
        std::process::exit(1);
    }
}
