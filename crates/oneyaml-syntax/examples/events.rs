//! Prints the event stream for a file (development aid):
//! `cargo run -p oneyaml-syntax --example events -- path/to/in.yaml`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

fn main() {
    let path = std::env::args().nth(1).expect("usage: events <file>");
    let src = std::fs::read_to_string(path).expect("readable UTF-8 file");
    let events = oneyaml_syntax::parse(&src);
    for e in events.sourced() {
        let text = e.text().unwrap_or("<bad span>");
        let span = e.span();
        match (e.diagnostic(), e.value()) {
            (Some(d), _) => println!("{:>5}..{:<5} {d}  {text:?}", span.start, span.end),
            (None, Some(value)) => println!(
                "{:>5}..{:<5} {:?} = {value:?}  {text:?}",
                span.start,
                span.end,
                e.kind()
            ),
            (None, None) => println!(
                "{:>5}..{:<5} {:?}  {text:?}",
                span.start,
                span.end,
                e.kind()
            ),
        }
    }
    match oneyaml_syntax::check_contract(&src, &events) {
        Ok(()) => println!("contract: ok"),
        Err(err) => println!("contract: {err}"),
    }
}
