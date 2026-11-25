//! Command-line control for a running brady-bunch host. A console program, so shells
//! wait for it and capture its output (brady-bunch.exe itself is a GUI program).
//!
//!     brady-ctl status [config|pid]
//!     brady-ctl start|stop|restart <app> [config|pid]
//!     brady-ctl tail <app> [config|pid] [--lines N]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        println!("{}", brady_bunch::control::USAGE);
        std::process::exit(if args.is_empty() { 1 } else { 0 });
    }
    std::process::exit(brady_bunch::control::client_main(&args));
}
