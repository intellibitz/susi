fn main() {
    let args = shlex::split("sh -c 'echo '\\''susi local models work definitively'\\'' > final_proof.txt'").unwrap();
    println!("{:?}", args);
}
