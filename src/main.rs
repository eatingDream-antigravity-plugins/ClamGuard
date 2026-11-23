mod cli;
mod guard;
mod hook;
mod mcp;
mod platform;
mod policy;
mod runner;
mod state;

fn main() {
    std::process::exit(cli::run());
}
