//! field: the Mru Field supervisor. It runs an operator's task as three
//! replicas on one processor, feeds every replica the same input, delivers
//! only outputs it has checked, retires replicas that fail, and resumes
//! after a reset or a power cut. See docs/spec.md and README.md.

mod check;
mod checkpoint;
mod config;
mod faults;
mod hash;
mod log;
mod protocol;
mod replica;
mod sink;
mod source;
mod stop;
mod summary;
mod supervisor;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const QUORUM_VERSION: &str = "0.4.0";

fn usage() -> ! {
    eprintln!(
        "usage: field run CONFIG\n       field check-task CONFIG [--requests N] [--seed S] [--inputs FILE]\n       field version"
    );
    std::process::exit(2)
}

fn load(path: &str) -> config::Config {
    match config::load(std::path::Path::new(path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("field: {e}");
            std::process::exit(2)
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("version") => {
            println!("field {VERSION} (quorum {QUORUM_VERSION})");
            0
        }
        Some("run") if args.len() == 2 => supervisor::run(load(&args[1])),
        Some("check-task") if args.len() >= 2 => {
            let cfg = load(&args[1]);
            let mut o = check::Options { requests: 10000, seed: 1, inputs: None };
            let mut it = args[2..].iter();
            while let Some(a) = it.next() {
                let mut value = || it.next().unwrap_or_else(|| usage());
                match a.as_str() {
                    "--requests" => o.requests = value().parse().unwrap_or_else(|_| usage()),
                    "--seed" => o.seed = value().parse().unwrap_or_else(|_| usage()),
                    "--inputs" => o.inputs = Some(value().into()),
                    _ => usage(),
                }
            }
            check::run(&cfg, &o)
        }
        _ => usage(),
    };
    std::process::exit(code)
}
