use std::process::ExitCode;

fn main() -> ExitCode {
    // Double-clicked (no arguments): keep the console window open at the end.
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let interactive = args.is_empty();
    patcher_stub::run_cli(args, interactive)
}
