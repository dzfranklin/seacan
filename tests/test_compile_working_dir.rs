use std::sync::Once;

use seacan::{bin, test};

// This needs to be a separate crate because the current working dir is per-process
fn set_cwd() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| std::env::set_current_dir("samples/hello_world").unwrap());
}

#[test]
fn test_bin() -> eyre::Result<()> {
    set_cwd();

    let artifact = bin::Compiler::bin("hello_world").compile()?;
    assert_eq!("hello_world", artifact.target.name);
    assert!(artifact.target.src_path.ends_with("src/main.rs"));
    Ok(())
}

#[test]
fn test_test() -> eyre::Result<()> {
    set_cwd();

    let mut artifacts = test::Compiler::new(
        test::NameSpec::substring("test_in_example_1"),
        test::TypeSpec::example("example_1"),
    )
    .compile()?;

    assert_eq!(1, artifacts.len());
    let mut artifact = artifacts.pop().unwrap();
    assert_eq!(1, artifact.tests.len());
    let test = artifact.tests.pop().unwrap();
    assert_eq!("test_in_example_1", test.name);
    assert_eq!(test::TestFnType::Test, test.test_type);

    Ok(())
}
