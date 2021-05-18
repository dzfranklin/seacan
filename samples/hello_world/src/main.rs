fn main() {
    println!("Hello, world!");
    panic!();
    println!("Warn this is unreachable");
}

#[test]
fn test_in_main() {}
