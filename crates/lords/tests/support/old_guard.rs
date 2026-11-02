//! Test support only: a guard copy from before this build's child protocol, as `tests/child.rs` installs one to check
//! sync's readiness across guard versions (fix check r2, FC4). It answers what `742a9bd`'s and `b0f5d93`'s copies
//! answer to the calls that matter: `ok` to the rm self-test (sync's old readiness check), and a usage note, exit 0 and
//! no context to `guard child … --home DIR`, the command line this build's sync writes; nothing to `guard rm …`, as a
//! copy from before D16 (tests/cli.rs).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["guard", "--self-test"] => println!("ok"),
        ["guard", "child", ..] => {
            eprintln!("lords guard child: usage `lords guard child [--part N]` (event on stdin)")
        }
        _ => {}
    }
}
