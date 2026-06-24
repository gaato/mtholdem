//! Scriptable command-line entrypoint for `mtholdem`.

#![forbid(unsafe_code)]

use std::env;
use std::process::ExitCode;

use mtholdem_adapter::{ABI_VERSION_MAJOR, ABI_VERSION_MINOR, ABI_VERSION_PATCH};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "version") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("abi-version") => {
            println!("{ABI_VERSION_MAJOR}.{ABI_VERSION_MINOR}.{ABI_VERSION_PATCH}");
            ExitCode::SUCCESS
        }
        Some("help" | "--help" | "-h") | None => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(command) => {
            eprintln!("unknown command: {command}");
            print_help();
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!("mtholdem {}", env!("CARGO_PKG_VERSION"));
    println!("usage: mtholdem <command>");
    println!();
    println!("commands:");
    println!("  abi-version   print the host-boundary ABI version");
    println!("  version       print the CLI package version");
}
