mod cli;
mod guard;
mod hook;
mod i18n;
mod launchagent;
mod mcp;
mod platform;
mod policy;
mod runner;
mod state;

fn main() {
    std::process::exit(cli::run());
}
