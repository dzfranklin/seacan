# seacan

[![Version 0.0.1](https://img.shields.io/crates/v/seacan)][crates-io]
[![License MIT](https://img.shields.io/crates/l/seacan)][crates-io]

A library for interacting with cargo to build things.

The main entrypoints are [`bin::Compiler`] and [`test::Compiler`].

## Binaries and examples

Building binaries and examples is relatively simple, although we do use
regexes to give you nicer errors in a few cases.

```rust
use seacan::bin;
let binary_artifact = bin::Compiler::bin("binary_name").release(true).compile()?;
let example_artifact = bin::Compiler::example("example_name").compile()?;
```

Example return value:

```rust
Ok(ExecutableArtifact {
    package_id: PackageId { .. },
    target: Target { .. },
    profile: ArtifactProfile { .. },
    features: [],
    filenames: [ .. ],
    executable: "/path/to/crate/.target/debug/example_name",
    fresh: true,
})
```

## Tests

Building tests is a bit more complicated. We expose all of Cargo's api for
specifying which test artifacts to build. After we build each artifact we
ask it for a list of all the test or benchmark functions in it that match
the spec you provided.

```rust
use seacan::test;
let mut artifacts = test::Compiler::new(
    test::NameSpec::exact("test_frobs_baz"),
    test::TypeSpec::integration("frob_*"),
).compile()?;
```

Example return value:

```rust
Ok(vec![
    Artifact {
        artifact: ExecutableArtifact {
            target: Target {
                name: "frob_a",
                ..
            },
            ...
        },
        tests: vec![
            TestFn {
                name: "test_frobs_baz",
                test_type: TestType::Test,
            },
        ],
    },
    Artifact {
        artifact: ExecutableArtifact {
            target: Target {
                name: "frob_b",
                ..
            },
            ...
        },
        tests: vec![],
    }
])
```

Only the default test runner (`libtest`) is supported.

## Why the name?

A Sea Can is another word for a shipping container. Shipping containers were
invented to provide a standard interface around handling cargo.

[crates-io]: https://crates.io/crates/seacan
