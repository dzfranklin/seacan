//!
//! Main entrypoint: [`bin::Compiler`]

use std::{
    io::BufReader,
    path::PathBuf,
    process::{Command, Stdio},
};

use camino::Utf8PathBuf;
use cargo_metadata::CompilerMessage;
use derivative::Derivative;
use tracing::instrument;

use crate::{
    handle_compiler_msg, BuildError, ExecutableArtifact, FeatureSpec, PackageSpec, MSG_FORMAT,
};

/// Compile a binary
///
/// ```
/// # use seacan::{bin::Compiler, FeatureSpec};
/// let artifact = Compiler::bin("hello_world")
///     .workspace("samples/hello_world")
///     .features(FeatureSpec::new(vec!["non_default_feature".into()]))
///     .release(true)
///     .compile()?;
/// # Ok::<_, seacan::BuildError>(())
#[derive(Derivative)]
#[derivative(Debug)]
pub struct Compiler {
    workspace: Option<PathBuf>,
    package: PackageSpec,
    name: String,
    is_example: bool,
    #[derivative(Debug = "ignore")]
    on_compiler_msg: Option<Box<dyn FnMut(CompilerMessage)>>,
    target_dir: Option<Utf8PathBuf>,
    features: Option<FeatureSpec>,
    is_release: bool,
}

impl Compiler {
    // TODO: fn default_bin
    //   See <https://github.com/rust-lang/cargo/issues/9491>

    /// Compile a binary.
    ///
    /// Note: By default the default binary has the name of the crate.
    #[must_use]
    pub fn bin(name: impl Into<String>) -> Self {
        Self::new(name, false)
    }

    /// Compile an example.
    #[must_use]
    pub fn example(name: impl Into<String>) -> Self {
        Self::new(name, true)
    }

    fn new(name: impl Into<String>, is_example: bool) -> Self {
        Self {
            workspace: None,
            package: PackageSpec::Any,
            name: name.into(),
            is_example,
            on_compiler_msg: None,
            target_dir: None,
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

    /// Compile the described executable
    #[instrument(err)]
    pub fn compile(&mut self) -> Result<ExecutableArtifact, BuildError> {
        let mut cmd = Command::new("cargo");

        cmd.arg("build")
            .arg(MSG_FORMAT)
            .args(&["--package", self.package.as_repr()])
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .stdin(Stdio::null());

        if let Some(features) = &self.features {
            cmd.args(features.to_args());
        }

        if let Some(ref workspace) = self.workspace {
            cmd.current_dir(workspace);
        }

        if self.is_release {
            cmd.arg("--release");
        }

        if let Some(ref target_dir) = self.target_dir {
            cmd.args(&["--target-dir", target_dir.as_str()]);
        }

        if self.is_example {
            cmd.args(&["--example", &self.name]);
        } else {
            cmd.args(&["--bin", &self.name]);
        }

        let mut cmd = cmd.spawn()?;

        let stdout = cmd.stdout.take().unwrap();
        let stderr = cmd.stderr.take().unwrap();

        let mut artifact = None;

        let messages = cargo_metadata::Message::parse_stream(BufReader::new(stdout));
        for msg in messages {
            match msg? {
                cargo_metadata::Message::CompilerMessage(msg) => {
                    handle_compiler_msg(msg, &mut self.on_compiler_msg)
                }
                cargo_metadata::Message::CompilerArtifact(art) => {
                    if art.executable.is_none() {
                        continue;
                    }
                    assert!(
                    artifact.is_none(),
                    "Expected cargo build with --bin or --example to only produce one executable"
                );
                    artifact = Some(art);
                }
                _ => {}
            }
        }

        if cmd.wait()?.success() {
            let artifact = artifact
                .expect("If cargo build exits with success should have built an executable");
            Ok(ExecutableArtifact::maybe_from(artifact).expect("Artifact has executable"))
        } else {
            Err(BuildError::from_stderr(stderr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_common::{init, Result};
    use pretty_assertions::{assert_eq, assert_ne};

    // TODO: Use assert_matches! when stable

    #[test]
    fn test_features() -> Result {
        let artifact = Compiler::bin("hello_world")
            .workspace("samples/hello_world")
            .features(FeatureSpec::new(vec!["non_default_feature".into()]))
            .compile()?;
        assert_eq!(
            vec![
                "default".to_string(),
                "default_feature".to_string(),
                "non_default_feature".to_string()
            ],
            artifact.features
        );
        Ok(())
    }

    #[test]
    fn test_no_default_features() -> Result {
        let artifact = Compiler::bin("hello_world")
            .workspace("samples/hello_world")
            .features(FeatureSpec::none())
            .compile()?;
        assert!(artifact.features.is_empty());
        Ok(())
    }

    #[test]
    fn test_release_false() -> Result {
        init();
        let artifact = Compiler::bin("hello_world")
            .workspace("samples/hello_world")
            .release(false)
            .compile()?;
        assert_eq!("0", artifact.profile.opt_level);
        Ok(())
    }
    #[test]
    fn test_release_true() -> Result {
        let artifact = Compiler::bin("hello_world")
            .workspace("samples/hello_world")
            .release(true)
            .compile()?;
        assert_ne!("0", artifact.profile.opt_level);
        Ok(())
    }

    #[test]
    fn test_release_default() -> Result {
        init();
        let artifact = Compiler::bin("hello_world")
            .workspace("samples/hello_world")
            .compile()?;
        assert_eq!("0", artifact.profile.opt_level);
        Ok(())
    }

    #[test]
    fn test_cargo_error() {
        init();
        let result = Compiler::bin("hello_world").workspace("/").compile();
        assert!(matches!(
            result,
            Err(BuildError::Cargo(stderr)) if stderr == "error: could not find `Cargo.toml` in `/` or any parent directory\n"
        ));
    }

    #[test]
    fn test_bin_main() -> Result {
        init();
        let artifact = Compiler::bin("hello_world")
            .workspace("samples/hello_world")
            .compile()?;
        assert_eq!("hello_world", artifact.target.name);
        assert!(artifact.target.src_path.ends_with("src/main.rs"));
        Ok(())
    }

    #[test]
    fn test_bin_2() -> Result {
        init();
        let artifact = Compiler::bin("bin_2")
            .workspace("samples/hello_world")
            .compile()?;
        assert_eq!("bin_2", artifact.target.name);
        assert!(artifact.target.src_path.ends_with("src/bin/bin_2.rs"));
        Ok(())
    }

    #[test]
    fn test_bin_nonexistent() {
        let result = Compiler::bin("bin_that_doesnt_exist")
            .workspace("samples/hello_world")
            .compile();
        assert!(matches!(result, Err(BuildError::NotFound(_))));
    }

    #[test]
    fn test_bin_nonexistent_package() {
        init();
        let result = Compiler::bin("bin_that_doesnt_exist")
            .package(PackageSpec::name("package_that_doesnt_exist"))
            .workspace("samples/hello_world")
            .compile();
        assert!(matches!(result, Err(BuildError::PackageNotFound(_))));
    }

    #[test]
    fn test_example() -> Result {
        init();
        let artifact = Compiler::example("example_1")
            .workspace("samples/hello_world")
            .compile()?;
        assert_eq!("example_1", artifact.target.name);
        assert!(artifact.target.src_path.ends_with("examples/example_1.rs"));
        Ok(())
    }

    #[test]
    fn test_example_nonexistent() {
        init();
        let result = Compiler::example("example_does_not_exist")
            .workspace("samples/hello_world")
            .compile();
        assert!(matches!(result, Err(BuildError::NotFound(_))));
    }

    #[test]
    fn test_example_nonexistent_package() {
        init();
        let result = Compiler::example("example_1")
            .workspace("samples/hello_world")
            .package(PackageSpec::name("nonexistent_package"))
            .compile();
        assert!(matches!(result, Err(BuildError::PackageNotFound(_))));
    }

    #[test]
    fn test_ws_member_main() -> Result {
        init();
        init();
        let artifact = Compiler::bin("ws_member")
            .package(PackageSpec::name("ws_member"))
            .workspace("samples/hello_world")
            .compile()?;
        assert_eq!("ws_member", artifact.target.name);
        assert!(artifact.target.src_path.ends_with("ws_member/src/main.rs"));
        Ok(())
    }
}
