mod cli;
mod index;
mod matcher;
mod mcp;
mod query;
mod scanner;
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
