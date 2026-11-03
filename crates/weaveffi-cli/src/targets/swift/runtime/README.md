# {{MODULE}} (Swift)

A SwiftPM package (product and module `{{MODULE}}`) over the native library
`{{LIBRARY}}`. Its C declarations are the binary target `{{C_MODULE}}`: a
prebuilt `{{C_MODULE}}.xcframework` of static libraries ({{SLICES}}), so apps
link the library with no flags or search paths.

## Publish

`Package.swift` resolves the binary target from

```text
{{URL}}
```

with checksum `{{CHECKSUM}}`. Upload `{{C_MODULE}}.xcframework.zip` (next to
this directory in the dist folder) to that URL, then commit this directory to
the repository your consumers depend on and tag the release. Set
`[generators.swift] xcframework_url` before packaging to choose the URL.

## Local development

A `{{C_MODULE}}.xcframework` next to `Package.swift` takes precedence over the
URL, so unzipping the archive here makes the package usable by path:

```bash
unzip ../{{C_MODULE}}.xcframework.zip -d .
```
