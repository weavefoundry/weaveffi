//! Gem packaging surfaces: the `.gemspec` (source-only and per-platform)
//! and the README.
//!
//! A gemspec is Ruby source, so every interpolated user string (summary,
//! authors, license, homepage) goes through the single-quote escape in
//! [`rb_str_literal`]; a quote or trailing backslash in package metadata
//! can't corrupt the emitted spec.

use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::pkg::Identity;

use crate::targets::ruby::types::rb_str_literal;

/// The names one generated gem is published and loaded under.
pub(crate) struct GemNames<'a> {
    /// The package identity.
    pub identity: &'a Identity,
    /// The gem name (`{name}` unless configured).
    pub gem: String,
    /// The top-level Ruby module (`PascalCase(name)` unless configured).
    pub module: String,
}

impl GemNames<'_> {
    /// The `require` path: the identity prefix.
    pub(crate) fn require(&self) -> &str {
        &self.identity.prefix
    }

    /// The gemspec file name.
    pub(crate) fn gemspec_file(&self) -> String {
        format!("{}.gemspec", self.gem)
    }
}

/// The `required_ruby_version` every gem declares.
pub(crate) const REQUIRED_RUBY_VERSION: &str = ">= 3.2";

/// The `ffi` gem requirement every gem declares.
pub(crate) const FFI_REQUIREMENT: &str = "~> 1.16";

/// The gem's authors. RubyGems requires a non-empty list, so an identity
/// without authors falls back to the gem name.
pub(crate) fn gem_authors(names: &GemNames) -> Vec<String> {
    let id = names.identity;
    if id.authors.is_empty() {
        vec![names.gem.clone()]
    } else {
        id.authors.clone()
    }
}

/// The metadata lines of the gemspec.
fn gemspec_metadata(names: &GemNames) -> String {
    let id = names.identity;
    let authors = gem_authors(names)
        .iter()
        .map(|a| format!("'{}'", rb_str_literal(a)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = format!(
        "  s.name        = '{}'\n  s.version     = '{}'\n",
        rb_str_literal(&names.gem),
        rb_str_literal(&id.version)
    );
    out.push_str(&format!(
        "  s.summary     = '{}'\n",
        rb_str_literal(&id.description_or_default())
    ));
    out.push_str(&format!("  s.authors     = [{authors}]\n"));
    if let Some(license) = &id.license {
        out.push_str(&format!(
            "  s.license     = '{}'\n",
            rb_str_literal(license)
        ));
    }
    if let Some(homepage) = id.homepage.as_ref().or(id.repository.as_ref()) {
        out.push_str(&format!(
            "  s.homepage    = '{}'\n",
            rb_str_literal(homepage)
        ));
    }
    out
}

/// Render the source-only gemspec emitted by `generate`.
pub(crate) fn render_gemspec(names: &GemNames) -> String {
    let prelude = render_prelude(CommentStyle::Hash);
    let trailer = render_trailer(CommentStyle::Hash, &names.gemspec_file());
    let meta = gemspec_metadata(names);
    format!(
        "{prelude}Gem::Specification.new do |s|
{meta}  s.files       = Dir['lib/**/*.rb']
  s.require_paths = ['lib']
  s.required_ruby_version = '{REQUIRED_RUBY_VERSION}'

  s.add_dependency 'ffi', '{FFI_REQUIREMENT}'
end

{trailer}"
    )
}

/// README for the source-only gem layout.
pub(crate) fn render_readme(names: &GemNames) -> String {
    let prelude = render_prelude(CommentStyle::Xml);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let gem = &names.gem;
    let version = &names.identity.version;
    let require = names.require();
    let module = &names.module;
    let env = names.identity.library_env_var();
    let (macos, linux, windows) = names.identity.library_files();
    format!(
        r#"{prelude}# {gem} (Ruby)

Ruby bindings for `{gem}` using the [ffi](https://github.com/ffi/ffi) gem.

## Prerequisites

- Ruby 3.2 or newer
- The native library (`{linux}`, `{macos}`, or `{windows}`) on the library
  search path, or its path in the `{env}` environment variable.

## Install

```bash
gem build {gem}.gemspec
gem install {gem}-{version}.gem
```

## Usage

```ruby
require '{require}'

{module}::ABI_VERSION
```

{trailer}"#
    )
}

/// README for a packaged Ruby platform gem.
pub(crate) fn render_packaged_readme(names: &GemNames) -> String {
    let prelude = render_prelude(CommentStyle::Xml);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let gem = &names.gem;
    let version = &names.identity.version;
    let require = names.require();
    format!(
        r#"{prelude}# {gem} (Ruby)

Ruby bindings for `{gem}` using the [ffi](https://github.com/ffi/ffi) gem,
with the native library bundled for this platform. The library loads
automatically; no external setup is required.

## Install

```bash
gem install {gem} -v {version}
```

## Usage

```ruby
require '{require}'
```

{trailer}"#
    )
}
