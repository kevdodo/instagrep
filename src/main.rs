mod cli;
mod daemon;
mod index;
mod matcher;
mod query;
mod scanner;
mod search;
mod trigram;

fn main() {
    match cli::run() {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
