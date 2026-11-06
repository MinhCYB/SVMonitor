use clap::Parser;

fn main() {
    let cli = svmonitor::cli::Cli::parse();
    match svmonitor::cli::main(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("svmonitor: {e:#}");
            std::process::exit(3);
        }
    }
}
