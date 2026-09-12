fn main() {
    let cli: skillforcer::cli::Cli = argh::from_env();
    match skillforcer::cli::dispatch(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("skillforcer: {e:#}");
            std::process::exit(0); // fail open: never block on our own error
        }
    }
}
