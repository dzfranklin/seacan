#![warn(clippy::all, clippy::pedantic, missing_docs, clippy::cargo)]

//! A library for interacting with cargo to build things.
//!
//! The main entrypoints are [`bin::Compiler`] and [`test::Compiler`].
//!
//! # Binaries and examples
//!
//! Building binaries and examples is relatively simple, although we do use
//! regexes to give you nicer errors in a few cases.
//!
//! ```
//! # fn _w() -> eyre::Result<()> {
//! use seacan::bin;
//! let binary_artifact = bin::Compiler::bin("binary_name").release(true).compile()?;
//! let example_artifact = bin::Compiler::example("example_name").compile()?;
//! # Ok(())
//! # }
//! ```
//!
//! Example return value:
//!
//! ```ignore
//! Ok(ExecutableArtifact {
//!     package_id: PackageId { .. },
//!     target: Target { .. },
//!     profile: ArtifactProfile { .. },
//!     features: [],
//!     filenames: [ .. ],
//!     executable: "/path/to/crate/.target/debug/example_name",
//!     fresh: true,
//! })
//! ```
//!
//! # Tests
//!
//! Building tests is a bit more complicated. We expose all of Cargo's api for
//! specifying which test artifacts to build. After we build each artifact we
//! ask it for a list of all the test or benchmark functions in it that match
//! the spec you provided.
//!
//! ```
//! # fn _w() -> eyre::Result<()> {
//! use seacan::test;
//! let mut artifacts = test::Compiler::new(
//!     test::NameSpec::exact("test_frobs_baz"),
//!     test::TypeSpec::integration("frob_*"),
//! ).compile()?;
//! # Ok(())
//! # }
//! ```
//!
//! Example return value:
//!
//! ```ignore
//! Ok(vec![
//!     Artifact {
//!         artifact: ExecutableArtifact {
//!             target: Target {
//!                 name: "frob_a",
//!                 ..
//!             },
//!             ...
//!         },
//!         tests: vec![
//!             TestFn {
//!                 name: "test_frobs_baz",
//!                 test_type: TestType::Test,
//!             },
//!         ],
//!     },
//!     Artifact {
//!         artifact: ExecutableArtifact {
//!             target: Target {
//!                 name: "frob_b",
//!                 ..
//!             },
//!             ...
//!         },
//!         tests: vec![],
//!     }
//! ])
//! ```
//!
//! Only the default test runner (`libtest`) is supported.
//!
//! # Why the name?
//!
//! A Sea Can is another word for a shipping container. Shipping containers were
//! invented to provide a standard interface around handling cargo.

/// Compile bins and examples (i.e. what you can `cargo run`)
pub mod bin;
/// Compile tests (unit tests in lib, doctests, integration tests, and unit
/// tests in bins and examples)
pub mod test;
#[cfg(test)]
mod test_common;

use std::{
    io::{self, Read},
    process::ChildStderr,
};

pub use camino::{Utf8Path, Utf8PathBuf};
pub use cargo_metadata::{
    diagnostic::{Diagnostic, DiagnosticLevel},
    ArtifactProfile, CompilerMessage, PackageId, Target,
};
use lazy_static::lazy_static;
use regex::Regex;
use tracing::{debug, info, instrument, warn};

/// Ensure the rendered field of JSON messages contains embedded ANSI color
/// codes for respecting rustc's default color scheme.
const MSG_FORMAT: &str = "--message-format=json-diagnostic-rendered-ansi";

/// Like [`cargo_metadata::Artifact`], but always has an executable
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ExecutableArtifact {
    /// The package this artifact belongs to
    pub package_id: PackageId,
    /// The target this artifact was compiled for
    pub target: Target,
    /// The profile this artifact was compiled with
    pub profile: ArtifactProfile,
    /// The enabled features for this artifact
    pub features: Vec<String>,
    /// The full paths to the generated artifacts
    /// (e.g. binary file and separate debug info)
    pub filenames: Vec<Utf8PathBuf>,
    /// Path to the executable file
    pub executable: Utf8PathBuf,
    /// If true, then the files were already generated
    pub fresh: bool,
}

impl ExecutableArtifact {
    fn maybe_from(art: cargo_metadata::Artifact) -> Option<Self> {
        let cargo_metadata::Artifact {
            package_id,
            target,
            profile,
            features,
            filenames,
            executable,
            fresh,
            ..
        } = art;

        Some(Self {
            package_id,
            target,
            profile,
            features,
            filenames,
            executable: executable?,
            fresh,
        })
    }
}

/// Describe a package (i.e. the `--package` flag)
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum PackageSpec {
    /// Any package in the workspace
    Any,
    /// The name of a package in the workspace
    Name(String),
    /// The full ID of a package in the workspace
    /// (i.e. `seacan 0.0.1 (path+file:///home/me/rdbg-proj/seacan)`).
    Id(PackageId),
}

impl PackageSpec {
    const ANY_REPR: &'static str = "*";

    /// Helper for [`Self::Name`]
    pub fn name(name: impl Into<String>) -> Self {
        Self::Name(name.into())
    }

    /// What you'd pass to to the `--package` flag.
    #[must_use]
    pub fn as_repr(&self) -> &str {
        match self {
            Self::Any => Self::ANY_REPR,
            Self::Name(repr) | Self::Id(PackageId { repr }) => repr,
        }
    }

    /// What you'd pass to to the `--package` flag.
    #[must_use]
    pub fn into_repr(self) -> String {
        match self {
            Self::Any => Self::ANY_REPR.to_owned(),
            Self::Name(repr) | Self::Id(PackageId { repr }) => repr,
        }
    }
}

impl From<PackageId> for PackageSpec {
    fn from(id: PackageId) -> Self {
        Self::Id(id)
    }
}

/// Describe a configuration of feature flags
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FeatureSpec(FeatureSpecInner);

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
enum FeatureSpecInner {
    Subset {
        include_default: bool,
        features: Vec<String>,
    },
    All,
}

impl FeatureSpec {
    /// `features`, on top of the default features (i.e. `--features ...`).
    #[must_use]
    pub fn new(features: Vec<String>) -> Self {
        Self(FeatureSpecInner::Subset {
            include_default: true,
            features,
        })
    }

    /// Only `features` (i.e. `--features ... --no-default-features`)
    #[must_use]
    pub fn new_no_default(features: Vec<String>) -> Self {
        Self(FeatureSpecInner::Subset {
            include_default: false,
            features,
        })
    }

    /// Every feature (i.e. `--all-features`)
    #[must_use]
    pub fn all() -> Self {
        Self(FeatureSpecInner::All)
    }

    /// Only the default features
    #[must_use]
    pub fn default_only() -> Self {
        Self::new(Vec::new())
    }

    /// No features (i.e. only `--no-default-features`)
    #[must_use]
    pub fn none() -> Self {
        Self::new_no_default(Vec::new())
    }

    /// Add a feature
    pub fn feature(&mut self, feature: String) -> &mut Self {
        match &mut self.0 {
            FeatureSpecInner::Subset { features, .. } => {
                features.push(feature);
            }
            FeatureSpecInner::All => {
                info!("Ignoring feature append as set to all")
            }
        }
        self
    }

    fn to_args(&self) -> Vec<String> {
        match &self.0 {
            FeatureSpecInner::All => vec!["--all-features".into()],
            FeatureSpecInner::Subset {
                include_default,
                features,
            } => {
                let mut args = Vec::new();
                if !features.is_empty() {
                    args.push("--features".into());
                    args.push(features.join(","));
                }
                if !include_default {
                    args.push("--no-default-features".into());
                }
                args
            }
        }
    }
}

pub(crate) fn handle_compiler_msg(
    msg: CompilerMessage,
    cb: &mut Option<Box<dyn FnMut(CompilerMessage)>>,
) {
    debug!(?msg, "Got compiler message");
    if let Some(cb) = cb {
        cb(msg)
    }
}

/// Failed to build
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum BuildError {
    /// Failed to run cargo
    RunCargo(#[from] io::Error),
    /// `{0}` not found
    NotFound(String),
    /// Package ID specification `{0:?}` did not match any packages
    PackageNotFound(String),
    /// Cargo build failed, stderr: {0}
    Cargo(String),
}

impl BuildError {
    #[instrument]
    fn from_stderr(mut stderr: ChildStderr) -> Self {
        let mut stderr_buf = String::new();
        if let Err(err) = stderr.read_to_string(&mut stderr_buf) {
            return Self::RunCargo(err);
        }

        lazy_static! {
            static ref NOT_FOUND_RE: Regex =
                Regex::new(r"error: no \w+ target named `(?P<n>.*?)`").unwrap();
            static ref PKG_NOT_FOUND_RE: Regex = Regex::new(
                r"error: package ID specification `(?P<p>.*?)` did not match any packages"
            )
            .unwrap();
        }

        #[allow(clippy::option_if_let_else)]
        if let Some(caps) = NOT_FOUND_RE.captures(&stderr_buf) {
            let name = caps.name("n").unwrap().as_str().to_owned();
            BuildError::NotFound(name)
        } else if let Some(caps) = PKG_NOT_FOUND_RE.captures(&stderr_buf) {
            let name = caps.name("p").unwrap().as_str().to_owned();
            BuildError::PackageNotFound(name)
        } else {
            BuildError::Cargo(stderr_buf)
        }
    }
}
