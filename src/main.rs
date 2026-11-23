use clap::Parser;
use ubuilder::cli::{self, Cli};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    match cli::run(cli).await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            cli::print_error(json, &error);
            std::process::exit(error.code.exit_code());
        }
    }
}
