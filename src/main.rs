mod cli;
mod index;
mod matcher;
mod query;
mod scanner;
mod trigram;

fn main() {
    match cli::run(std::env::args().skip(1)) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
