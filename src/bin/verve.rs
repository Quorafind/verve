//! `verve`: run a script as Node runs one; see [`verve::cli`].

fn main() -> std::process::ExitCode {
    verve::cli::main(std::env::args_os())
}
