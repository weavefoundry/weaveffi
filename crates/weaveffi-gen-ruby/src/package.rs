//! Gem packaging surfaces: the `.gemspec` (plain and per-platform) and the
//! README, for both `generate` and `package` layouts.
//!
//! A gemspec is Ruby source, so every interpolated user string (summary,
//! authors, license, homepage) goes through the single-quote escape in
//! [`rb_str_literal`]; a quote or trailing backslash in package metadata
//! cannot corrupt the emitted spec.

use weaveffi_core::pkg::ResolvedPackage;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::types::rb_str_literal;

/// The `s.authors`/`s.license`/`s.homepage` block shared by both gemspec
/// shapes, one line per present optional field.
fn gemspec_extra(package: &ResolvedPackage) -> String {
    let mut extra = String::new();
    if !package.authors.is_empty() {
        let authors = package
            .authors
            .iter()
            .map(|a| format!("'{}'", rb_str_literal(a)))
            .collect::<Vec<_>>()
            .join(", ");
        extra.push_str(&format!("  s.authors     = [{authors}]\n"));
    }
    if let Some(license) = &package.license {
        extra.push_str(&format!(
            "  s.license     = '{}'\n",
            rb_str_literal(license)
        ));
    }
    if let Some(homepage) = package.homepage.as_ref().or(package.repository.as_ref()) {
        extra.push_str(&format!(
            "  s.homepage    = '{}'\n",
            rb_str_literal(homepage)
        ));
    }
    extra
}

/// Render the source-only gemspec emitted by `generate`.
pub(crate) fn render_gemspec(
    package: &ResolvedPackage,
    gem_file: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, gem_file);
    let name = &package.name;
    let version = &package.version;
    let summary = rb_str_literal(&package.description_or_default());
    let extra = gemspec_extra(package);
    format!(
        "{prelude}Gem::Specification.new do |s|
  s.name        = '{name}'
  s.version     = '{version}'
  s.summary     = '{summary}'
{extra}  s.files       = Dir['lib/**/*.rb']
  s.require_paths = ['lib']

  s.add_dependency 'ffi', '~> 1.15'
end

{trailer}"
    )
}

/// Render a platform gemspec: it stamps `s.platform` with the RubyGems
/// platform string (`Platform::ruby_platform`) and ships the bundled native
/// library alongside the Ruby sources.
pub(crate) fn render_packaged_gemspec(
    package: &ResolvedPackage,
    gem_file: &str,
    ruby_platform: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, gem_file);
    let name = &package.name;
    let version = &package.version;
    let summary = rb_str_literal(&package.description_or_default());
    let extra = gemspec_extra(package);
    format!(
        "{prelude}Gem::Specification.new do |s|
  s.name        = '{name}'
  s.version     = '{version}'
  s.platform    = '{ruby_platform}'
  s.summary     = '{summary}'
{extra}  s.files       = Dir['lib/**/*.rb'] + Dir['lib/**/*.{{so,dylib,dll}}']
  s.require_paths = ['lib']

  s.add_dependency 'ffi', '~> 1.15'
end

{trailer}"
    )
}

/// README for the source-only gem layout.
pub(crate) fn render_readme(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let version = &package.version;
    let require_name = package.ident_name();
    format!(
        r#"{prelude}# {name} (Ruby)

Auto-generated Ruby bindings using the [ffi](https://github.com/ffi/ffi) gem.

## Prerequisites

- Ruby >= 2.7
- The compiled shared library (`libweaveffi.so`, `libweaveffi.dylib`, or `weaveffi.dll`) available on your library search path.

## Install

```bash
gem build {name}.gemspec
gem install {name}-{version}.gem
```

## Usage

```ruby
require '{require_name}'
```

{trailer}"#
    )
}

/// README for a packaged Ruby platform gem.
pub(crate) fn render_packaged_readme(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let version = &package.version;
    let require_name = package.ident_name();
    format!(
        r#"{prelude}# {name} (Ruby)

Auto-generated Ruby bindings using the [ffi](https://github.com/ffi/ffi) gem,
with the native library bundled for this platform. The library loads
automatically; no external setup is required.

## Install

```bash
gem build {name}.gemspec
gem install {name}-{version}-*.gem
```

## Usage

```ruby
require '{require_name}'
```

{trailer}"#
    )
}
