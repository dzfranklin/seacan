//!
//! Main entrypoint: [`test::Compiler`]

use std::{
    fmt,
    io::{self, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
};

use camino::Utf8PathBuf;
use cargo_metadata::CompilerMessage;
use derivative::Derivative;
use lazy_static::lazy_static;
use regex::Regex;
use tracing::{error, instrument, warn};

use crate::{
    handle_compiler_msg, BuildError, ExecutableArtifact, FeatureSpec, PackageSpec, MSG_FORMAT,
};

/// Compile tests
///
/// ```
/// # use seacan::{test::{Compiler, NameSpec, TypeSpec}, FeatureSpec};
/// let artifacts = Compiler::new(NameSpec::substring("test_in_lib_1"), TypeSpec::Lib)
///     .features(FeatureSpec::new(vec!["non_default_feature".into()]))
///     .workspace("samples/hello_world")
///     .compile()?;
/// # Ok::<_, seacan::test::Error>(())
/// ```
#[derive(Derivative)]
#[derivative(Debug)]
pub struct Compiler {
    target_dir: Option<Utf8PathBuf>,
    workspace: Option<PathBuf>,
    package: PackageSpec,
    name: NameSpec,
    test_type: TypeSpec,
    #[derivative(Debug = "ignore")]
    on_compiler_msg: Option<Box<dyn FnMut(CompilerMessage)>>,
    features: Option<FeatureSpec>,
    is_release: bool,
}

/// A compiled test artifact
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Artifact {
    /// Details of the artifact
    pub artifact: ExecutableArtifact,
    /// The specific tests and benches in the artifact that match the spec
    /// you provided.
    pub tests: Vec<TestFn>,
    name_spec: NameSpec,
}

impl Artifact {
    /// The arguments you should provide to the test artifact if you want to run
    /// only the tests and benches that match the spec you provided.
    #[must_use]
    pub fn run_args(&self) -> Vec<String> {
        self.name_spec.run_args()
    }
}

/// A test or bench in a compiled artifact.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
#[allow(clippy::module_name_repetitions)]
pub struct TestFn {
    /// The name of the test
    pub name: String,
    /// The type of the test
    pub test_type: TestFnType,
}

impl TestFn {
    /// The arguments you should provide to the test artifact if you want to run
    /// only this test or bench.
    #[must_use]
    pub fn run_args(&self) -> Vec<String> {
        NameSpec::exact_run_args(self.name.clone())
    }
}

/// Whether this test function is a test or benchmark.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
#[allow(clippy::module_name_repetitions)]
pub enum TestFnType {
    /// A test (created with `#[test]`)
    Test,
    /// A bench (unstable, created with `#[bench]`)
    Bench,
}

impl fmt::Display for TestFnType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Test => write!(f, "test"),
            Self::Bench => write!(f, "bench"),
        }
    }
}

/// Specify tests and benches based on their name
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum NameSpec {
    /// Only exact matches (i.e. `cargo test -- --exact`)
    Exact(String),
    /// Matches anything with the substring in it (the default behavior of
    /// `cargo test`)
    Substring(String),
    /// Matches every test and bench
    Any,
}

impl NameSpec {
    /// Helper for [`Self::Exact`]
    #[must_use]
    pub fn exact(s: impl Into<String>) -> Self {
        Self::Exact(s.into())
    }

    /// Helper for [`Self::substring`]
    #[must_use]
    pub fn substring(s: impl Into<String>) -> Self {
        Self::Substring(s.into())
    }

    fn run_args(&self) -> Vec<String> {
        match self {
            NameSpec::Exact(name) => Self::exact_run_args(name.clone()),
            NameSpec::Substring(name) => vec![name.into()],
            NameSpec::Any => vec![],
        }
    }

    fn exact_run_args(name: String) -> Vec<String> {
        vec!["--exact".into(), name]
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
/// Specify the type of test artifact to build
///
/// Note: The names can contain globs (eg `bin_*`).
pub enum TypeSpec {
    /// Unit tests in the library.
    ///
    /// By default this is `lib.rs` and any modules defined in it.
    Lib,
    /// Unit tests defined in a binary
    Bin(String),
    /// Unit tests defined in any binary
    Bins,
    /// Integration tests (i.e. `cargo test --test <name>`)
    Integration(String),
    /// Every integration test
    Integrations,
    /// Unit tests defined in an examply
    Example(String),
    /// Unit tests defined in any example
    Examples,
    /// Doctests
    Doc,
    /// All tests
    All,
}

impl TypeSpec {
    /// Helper for [`Self::Bin`]
    #[must_use]
    pub fn bin(name: impl Into<String>) -> Self {
        Self::Bin(name.into())
    }

    /// Helper for [`Self::Integration`]
    #[must_use]
    pub fn integration(name: impl Into<String>) -> Self {
        Self::Integration(name.into())
    }

    /// Helper for [`Self::Example`]
    #[must_use]
    pub fn example(name: impl Into<String>) -> Self {
        Self::Example(name.into())
    }
}

impl Compiler {
    /// Describe tests to be compiled
    #[must_use]
    pub fn new(name: NameSpec, test_type: TypeSpec) -> Self {
        Self {
            workspace: None,
            package: PackageSpec::Any,
            name,
            on_compiler_msg: None,
            target_dir: None,
            test_type,
            features: None,
            is_release: false,
        }
    }

    /// The directory to run cargo in.
    ///
    /// By default the current working directory.
    pub fn workspace(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.workspace = Some(path.into());
        self
    }

    /// The package the binary is in.
    ///
    /// By default [`PackageSpec::Any`].
    pub fn package(&mut self, package: PackageSpec) -> &mut Self {
        self.package = package;
        self
    }

    /// Callback for compiler messages.
    ///
    /// Regardless of if you specify this compiler messages will be logged at
    /// debug level using [`tracing`].
    pub fn on_compiler_msg(&mut self, cb: impl FnMut(CompilerMessage) + 'static) -> &mut Self {
        self.on_compiler_msg = Some(Box::new(cb));
        self
    }

    /// Where to put the build artifacts.
    ///
    /// By default this is whatever cargo chooses by default.
    pub fn target_dir(&mut self, target_dir: impl Into<Utf8PathBuf>) -> &mut Self {
        self.target_dir = Some(target_dir.into());
        self
    }

    /// Enable or disable feature flags.
    ///
    /// By default this is whatever cargo chooses by default.
    pub fn features(&mut self, features: FeatureSpec) -> &mut Self {
        self.features = Some(features);
        self
    }

    /// If we should build in release mode.
    pub fn release(&mut self, is_release: bool) -> &mut Self {
        self.is_release = is_release;
        self
    }

    /// Compile the described tests
    #[instrument(err)]
    pub fn compile(&mut self) -> Result<Vec<Artifact>, Error> {
        self.artifacts_ignoring_name()?
            .into_iter()
            .map(|artifact| self.get_artifact_tests(artifact))
            .collect()
    }

    #[instrument(err)]
    fn get_artifact_tests(&self, artifact: ExecutableArtifact) -> Result<Artifact, Error> {
        // TODO: If json format is added use it <https://github.com/rust-lang/libtest/issues/23>

        let mut cmd = Command::new(&artifact.executable);

        cmd.arg("--list")
            .arg("--format=terse")
            .args(&self.name.run_args())
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .stdin(Stdio::null());

        if let Some(ref workspace) = self.workspace {
            cmd.current_dir(workspace);
        }

        let out = cmd.spawn()?.wait_with_output()?;

        if !out.status.success() {
            return Err(Error::Libtest(String::from_utf8_lossy(&out.stderr).into()));
        }

        let stdout = String::from_utf8(out.stdout).map_err(|err| {
            error!("test binary stdout not utf-8: {}", err);
            Error::Parse(String::from_utf8_lossy(&err.as_bytes()).into())
        })?;

        let tests = parse_libtest_stdout(&stdout)?;
        Ok(Artifact {
            artifact,
            tests,
            name_spec: self.name.clone(),
        })
    }

    #[instrument(err)]
    fn artifacts_ignoring_name(&mut self) -> Result<Vec<ExecutableArtifact>, BuildError> {
        let mut cmd = Command::new("cargo");

        cmd.arg("test")
            .arg("--no-run")
            .arg(MSG_FORMAT)
            .args(&["--package", self.package.as_repr()])
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .stdin(Stdio::null());

        if let Some(ref workspace) = self.workspace {
            cmd.current_dir(workspace);
        }

        if let Some(features) = &self.features {
            cmd.args(&features.to_args());
        }

        if self.is_release {
            cmd.arg("--release");
        }

        if let Some(ref target_dir) = self.target_dir {
            cmd.args(&["--target-dir", target_dir.as_str()]);
        }

        match &self.test_type {
            TypeSpec::Lib => cmd.arg("--lib"),
            TypeSpec::Bin(name) => cmd.args(&["--bin", name]),
            TypeSpec::Bins => cmd.arg("--bins"),
            TypeSpec::Integration(name) => cmd.args(&["--test", name]),
            TypeSpec::Integrations => cmd.args(&["--test", "*"]),
            TypeSpec::Doc => cmd.arg("--doc"),
            TypeSpec::Example(name) => cmd.args(&["--example", name]),
            TypeSpec::Examples => cmd.arg("--examples"),
            TypeSpec::All => &mut cmd,
        };

        let mut cmd = cmd.spawn()?;

        let stdout = cmd.stdout.take().unwrap();
        let stderr = cmd.stderr.take().unwrap();

        let mut artifacts = Vec::new();

        let messages = cargo_metadata::Message::parse_stream(BufReader::new(stdout));
        for msg in messages {
            match msg? {
                cargo_metadata::Message::CompilerMessage(msg) => {
                    handle_compiler_msg(msg, &mut self.on_compiler_msg)
                }
                cargo_metadata::Message::CompilerArtifact(art) => {
                    if !art.profile.test {
                        // cargo --test builds binaries so that integration tests can run them.
                        // See <https://github.com/rust-lang/cargo/issues/7958>
                        continue;
                    }
                    if let Some(art) = ExecutableArtifact::maybe_from(art) {
                        artifacts.push(art);
                    }
                }
                _ => {}
            }
        }

        if cmd.wait()?.success() {
            Ok(artifacts)
        } else {
            Err(BuildError::from_stderr(stderr))
        }
    }
}

#[instrument(err)]
fn parse_libtest_stdout(stdout: &str) -> Result<Vec<TestFn>, Error> {
    // See libtest::list_tests_console
    // <https://github.com/rust-lang/libtest/blob/master/libtest/lib.rs#L837>

    lazy_static! {
        static ref LINE_RE: Regex = Regex::new(r"^(?P<n>.*): (?P<t>.*)$").unwrap();
    }

    let mut tests = Vec::new();
    for line in stdout.lines() {
        let caps = LINE_RE
            .captures(line)
            .ok_or_else(|| Error::Parse(stdout.to_string()))?;

        let name = caps.name("n").unwrap().as_str().to_owned();
        let test_type = match caps.name("t").unwrap().as_str() {
            "test" => TestFnType::Test,
            "benchmark" => TestFnType::Bench,
            other => {
                warn!(?name, "Ignoring unsupported test type `{}`", other);
                continue;
            }
        };

        tests.push(TestFn { name, test_type });
    }
    Ok(tests)
}

/// Failed to build tests
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// Failed to build
    Build(#[from] BuildError),
    /// Failed to execute `<test_binary> --list`
    Execute(#[from] io::Error),
    /// `<test_binary> --list` returned failure. Are you using a custom test runner? Stderr: {0}
    Libtest(String),
    /// Failed to parse stdout of `<test_binary> --list`. Are you using a custom test runner? Got: {0}
    Parse(String),
}

#[cfg(test)]
mod tests {
    // TODO: Use assert_matches! when stable

    use super::*;
    use crate::test_common::{init, Result};
    use pretty_assertions::assert_eq;

    #[test]
    fn test_disabled_feature() -> Result {
        init();
        let mut artifacts =
            Compiler::new(NameSpec::exact("test_non_default_feature"), TypeSpec::Lib)
                .workspace("samples/hello_world")
                .compile()?;
        assert_eq!(1, artifacts.len());
        let artifact = artifacts.pop().unwrap();
        assert_eq!(0, artifact.tests.len());
        Ok(())
    }

    #[test]
    fn test_enabled_feature() -> Result {
        init();
        let mut artifacts =
            Compiler::new(NameSpec::exact("test_non_default_feature"), TypeSpec::Lib)
                .features(FeatureSpec::new(vec!["non_default_feature".into()]))
                .workspace("samples/hello_world")
                .compile()?;
        assert_eq!(1, artifacts.len());
        let artifact = artifacts.pop().unwrap();
        assert_eq!(1, artifact.tests.len());
        Ok(())
    }

    #[test]
    fn test_no_default_features() -> Result {
        init();
        let mut artifacts = Compiler::new(NameSpec::exact("test_default_feature"), TypeSpec::Lib)
            .workspace("samples/hello_world")
            .features(FeatureSpec::none())
            .compile()?;
        assert_eq!(1, artifacts.len());
        let artifact = artifacts.pop().unwrap();
        assert_eq!(0, artifact.tests.len());
        Ok(())
    }

    #[test]
    fn test_release_false() -> Result {
        init();
        let artifact = Compiler::new(NameSpec::Any, TypeSpec::bin("hello_world"))
            .workspace("samples/hello_world")
            .release(false)
            .compile()?
            .pop()
            .unwrap();
        assert_eq!("0", artifact.artifact.profile.opt_level);
        Ok(())
    }

    #[test]
    fn test_release_true() -> Result {
        init();
        let artifact = Compiler::new(NameSpec::Any, TypeSpec::bin("hello_world"))
            .workspace("samples/hello_world")
            .release(true)
            .compile()?
            .pop()
            .unwrap();
        assert_ne!("0", artifact.artifact.profile.opt_level);
        Ok(())
    }

    #[test]
    fn test_release_default() -> Result {
        init();
        let artifact = Compiler::new(NameSpec::Any, TypeSpec::bin("hello_world"))
            .workspace("samples/hello_world")
            .compile()?
            .pop()
            .unwrap();
        assert_eq!("0", artifact.artifact.profile.opt_level);
        Ok(())
    }

    #[test]
    fn test_artifact_run_args() -> Result {
        init();
        let mut artifacts = Compiler::new(
            NameSpec::substring("test_in_example_1"),
            TypeSpec::example("example_1"),
        )
        .workspace("samples/hello_world")
        .compile()?;
        let mut artifact = artifacts.pop().unwrap();
        assert_eq!(vec!["test_in_example_1".to_string()], artifact.run_args());
        let test_fn = artifact.tests.pop().unwrap();
        assert_eq!(
            vec!["--exact".to_string(), "test_in_example_1".to_string()],
            test_fn.run_args()
        );
        Ok(())
    }

    #[test]
    fn test_all() -> Result {
        init();

        let artifacts = Compiler::new(NameSpec::substring("test_in_lib"), TypeSpec::All)
            .workspace("samples/hello_world")
            .compile()?;

        let tests: Vec<TestFn> = artifacts.into_iter().flat_map(|a| a.tests).collect();
        assert_eq!(2, tests.len());

        Ok(())
    }

    #[test]
    fn test_in_example() -> Result {
        init();

        let mut artifacts = Compiler::new(
            NameSpec::substring("test_in_example_1"),
            TypeSpec::example("example_1"),
        )
        .workspace("samples/hello_world")
        .compile()?;

        assert_eq!(1, artifacts.len());
        let mut artifact = artifacts.pop().unwrap();
        assert_eq!(1, artifact.tests.len());
        let test = artifact.tests.pop().unwrap();
        assert_eq!("test_in_example_1", test.name);
        assert_eq!(TestFnType::Test, test.test_type);

        Ok(())
    }

    #[test]
    fn test_bin_2() -> Result {
        init();

        let mut artifacts = Compiler::new(NameSpec::Any, TypeSpec::bin("bin_2"))
            .workspace("samples/hello_world")
            .compile()?;

        assert_eq!(1, artifacts.len());
        let mut artifact = artifacts.pop().unwrap();
        assert_eq!(1, artifact.tests.len());
        let test = artifact.tests.pop().unwrap();
        assert_eq!("test_in_bin_2", test.name);
        assert_eq!(TestFnType::Test, test.test_type);

        Ok(())
    }

    #[test]
    fn test_main() -> Result {
        init();

        let mut artifacts = Compiler::new(NameSpec::Any, TypeSpec::bin("hello_world"))
            .workspace("samples/hello_world")
            .compile()?;

        assert_eq!(1, artifacts.len());
        let mut artifact = artifacts.pop().unwrap();
        assert_eq!(1, artifact.tests.len());
        let test = artifact.tests.pop().unwrap();
        assert_eq!("test_in_main", test.name);
        assert_eq!(TestFnType::Test, test.test_type);

        Ok(())
    }

    #[test]
    fn test_lib() -> Result {
        init();

        let mut artifacts = Compiler::new(NameSpec::substring("test_in_lib_2"), TypeSpec::Lib)
            .workspace("samples/hello_world")
            .compile()?;

        assert_eq!(1, artifacts.len());
        let mut artifact = artifacts.pop().unwrap();
        assert_eq!(1, artifact.tests.len());
        let test = artifact.tests.pop().unwrap();
        assert_eq!("test_in_lib_2", test.name);
        assert_eq!(TestFnType::Test, test.test_type);

        Ok(())
    }

    #[test]
    fn test_module() -> Result {
        init();

        let mut artifacts = Compiler::new(NameSpec::substring("test_in_module"), TypeSpec::Lib)
            .workspace("samples/hello_world")
            .compile()?;

        assert_eq!(1, artifacts.len());
        let mut artifact = artifacts.pop().unwrap();
        assert_eq!(1, artifact.tests.len());
        let test = artifact.tests.pop().unwrap();
        assert_eq!("module::test_in_module", test.name);
        assert_eq!(TestFnType::Test, test.test_type);

        Ok(())
    }

    #[test]
    fn test_integration() -> Result {
        init();

        let mut artifacts = Compiler::new(
            NameSpec::substring("integration_tests_1_test"),
            TypeSpec::integration("integration_tests_1"),
        )
        .workspace("samples/hello_world")
        .compile()?;

        assert_eq!(1, artifacts.len());
        let mut artifact = artifacts.pop().unwrap();
        assert_eq!(1, artifact.tests.len());
        let test = artifact.tests.pop().unwrap();
        assert_eq!("integration_tests_1_test", test.name);
        assert_eq!(TestFnType::Test, test.test_type);

        Ok(())
    }

    #[test]
    fn test_partial_name() -> Result {
        init();

        let mut artifacts = Compiler::new(NameSpec::substring("in_lib_1"), TypeSpec::Lib)
            .workspace("samples/hello_world")
            .compile()?;

        assert_eq!(1, artifacts.len());
        let mut artifact = artifacts.pop().unwrap();
        assert_eq!(1, artifact.tests.len());
        let test = artifact.tests.pop().unwrap();
        assert_eq!("test_in_lib_1", test.name);
        assert_eq!(TestFnType::Test, test.test_type);

        Ok(())
    }

    #[test]
    fn test_exact_name() -> Result {
        init();

        let mut artifacts = Compiler::new(NameSpec::substring("test_in_lib_1"), TypeSpec::Lib)
            .workspace("samples/hello_world")
            .compile()?;

        assert_eq!(1, artifacts.len());
        let mut artifact = artifacts.pop().unwrap();
        assert_eq!(1, artifact.tests.len());
        let test = artifact.tests.pop().unwrap();
        assert_eq!("test_in_lib_1", test.name);
        assert_eq!(TestFnType::Test, test.test_type);

        Ok(())
    }

    #[test]
    fn test_any_name() -> Result {
        init();

        let mut artifacts = Compiler::new(NameSpec::Any, TypeSpec::Lib)
            .workspace("samples/hello_world")
            .compile()?;

        assert_eq!(1, artifacts.len());
        let mut artifact = artifacts.pop().unwrap();
        artifact.tests.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(4, artifact.tests.len());
        let test_1 = &artifact.tests[0];
        let test_2 = &artifact.tests[1];
        let test_3 = &artifact.tests[2];
        let test_4 = &artifact.tests[3];

        assert_eq!("module::test_in_module", test_1.name);
        assert_eq!("test_default_feature", test_2.name);
        assert_eq!("test_in_lib_1", test_3.name);
        assert_eq!("test_in_lib_2", test_4.name);

        Ok(())
    }

    #[test]
    fn test_multiple_artifacts() -> Result {
        init();

        let mut artifacts = Compiler::new(NameSpec::Any, TypeSpec::Integrations)
            .workspace("samples/hello_world")
            .compile()?;
        artifacts.sort_by(|a, b| a.artifact.target.src_path.cmp(&b.artifact.target.src_path));

        assert_eq!(2, artifacts.len());
        let integration_1 = &artifacts[0];
        let integration_2 = &artifacts[1];

        assert_eq!(1, integration_1.tests.len());
        assert_eq!("integration_tests_1_test", &integration_1.tests[0].name);

        assert_eq!(1, integration_2.tests.len());
        assert_eq!("integration_tests_2_test", &integration_2.tests[0].name);

        Ok(())
    }

    #[test]
    fn test_ws_member() -> Result {
        init();

        let mut artifacts = Compiler::new(NameSpec::Any, TypeSpec::bin("ws_member"))
            .workspace("samples/hello_world")
            .package(PackageSpec::name("ws_member"))
            .compile()?;

        assert_eq!(1, artifacts.len());
        let mut artifact = artifacts.pop().unwrap();
        assert_eq!(1, artifact.tests.len());
        let test = artifact.tests.pop().unwrap();
        assert_eq!("test_in_ws_member_main", test.name);
        assert_eq!(TestFnType::Test, test.test_type);

        Ok(())
    }
}
