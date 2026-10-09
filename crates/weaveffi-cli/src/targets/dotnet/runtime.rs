//! The fixed runtime (`runtime/Runtime.cs`, spliced with the library's
//! names) and the names of its public classes.

use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::errors;
use weaveffi_model::model::{Model, ABI_VERSION};

/// The runtime source shared by every generated library.
const RUNTIME_CS: &str = include_str!("runtime/Runtime.cs");

/// The public classes of the runtime, named so they never collide with a
/// type the API declares.
pub(crate) struct RuntimeNames {
    /// The root of every error a throwing call reports (`NativeException`).
    pub exception: String,
    /// The trap a failed non-throwing call raises (`NativeBugException`).
    pub bug_exception: String,
    /// The load-time failure (`NativeLoadException`).
    pub load_exception: String,
    /// The static class with `Check()` (`{Namespace}Library`).
    pub library_class: String,
}

impl RuntimeNames {
    /// The names for `model` in `namespace`. A name the API already declares
    /// is qualified by the namespace's last segment (`KvstoreNativeException`).
    pub(crate) fn new(model: &Model, namespace: &str) -> Self {
        let taken = |name: &str| {
            model.modules.iter().any(|m| {
                m.structs.iter().any(|s| s.name == name)
                    || m.enums.iter().any(|e| e.name == name)
                    || m.interfaces.iter().any(|i| i.name == name)
                    || m.callback_interfaces
                        .iter()
                        .any(|c| c.name == name || format!("I{}", c.name) == name)
                    || m.errors
                        .iter()
                        .any(|e| errors::exception_type_name(&e.name) == name)
            })
        };
        let last = namespace.rsplit('.').next().unwrap_or(namespace);
        let pick = |name: &str| {
            if taken(name) {
                format!("{last}{name}")
            } else {
                name.to_string()
            }
        };
        let library = format!("{last}Library");
        Self {
            exception: pick("NativeException"),
            bug_exception: pick("NativeBugException"),
            load_exception: pick("NativeLoadException"),
            library_class: if taken(&library) {
                format!("{last}NativeLibrary")
            } else {
                library
            },
        }
    }
}

/// Render `Runtime.cs`, loading `library`: the fixed runtime with every
/// placeholder replaced.
pub(crate) fn render_runtime(
    model: &Model,
    namespace: &str,
    names: &RuntimeNames,
    library: &str,
    filename: &str,
) -> String {
    let identity = &model.identity;
    let body = RUNTIME_CS
        .replace("{{NAMESPACE}}", namespace)
        .replace("{{BUG_EXCEPTION}}", &names.bug_exception)
        .replace("{{LOAD_EXCEPTION}}", &names.load_exception)
        .replace("{{EXCEPTION}}", &names.exception)
        .replace("{{LIBRARY_CLASS}}", &names.library_class)
        .replace("{{PREFIX}}", model.prefix())
        .replace("{{LIBRARY_ENV}}", &identity.library_env_var())
        .replace("{{LIBRARY}}", library)
        .replace("{{ABI_VERSION}}", &ABI_VERSION.to_string());
    debug_assert!(!body.contains("{{"), "unfilled placeholder in Runtime.cs");
    format!(
        "{}{body}\n{}",
        render_prelude(CommentStyle::DoubleSlash),
        render_trailer(CommentStyle::DoubleSlash, filename)
    )
}
