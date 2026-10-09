//! The language generators, one module per target. Each implements
//! [`LanguageBackend`](crate::backend::LanguageBackend) and exposes its
//! generator and configuration types (for example
//! [`swift::SwiftGenerator`] and [`swift::SwiftConfig`]).

pub mod c;
pub mod cpp;
pub mod dart;
pub mod dotnet;
pub mod go;
pub(crate) mod js;
pub mod kotlin;
pub mod node;
pub mod python;
pub mod ruby;
pub mod swift;
pub mod wasm;

use crate::codegen::{ConfiguredBackend, Target};

/// Every target with its default configuration, in canonical order (the
/// order of the `--target` tokens).
#[must_use]
pub fn all_default() -> Vec<Box<dyn Target>> {
    vec![
        Box::new(ConfiguredBackend::new(c::CGenerator, c::CConfig::default())),
        Box::new(ConfiguredBackend::new(
            cpp::CppGenerator,
            cpp::CppConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(
            swift::SwiftGenerator,
            swift::SwiftConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(
            kotlin::KotlinGenerator,
            kotlin::KotlinConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(
            node::NodeGenerator,
            node::NodeConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(
            wasm::WasmGenerator,
            wasm::WasmConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(
            python::PythonGenerator,
            python::PythonConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(
            dotnet::DotnetGenerator,
            dotnet::DotnetConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(
            dart::DartGenerator,
            dart::DartConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(
            go::GoGenerator,
            go::GoConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(
            ruby::RubyGenerator,
            ruby::RubyConfig::default(),
        )),
    ]
}
