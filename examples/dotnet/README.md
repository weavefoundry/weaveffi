# .NET Examples

## Prerequisites

- [.NET 8 SDK](https://dotnet.microsoft.com/download/dotnet/8.0) installed
- Rust toolchain installed

## Contacts

1. Build the contacts library and generate .NET bindings (from repo root):

```bash
cargo build -p contacts
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated
```

2. Build the example:

```bash
cd examples/dotnet/Contacts
dotnet build
```

3. Run:

```bash
# macOS
DYLD_LIBRARY_PATH=../../../target/debug dotnet run

# Linux
LD_LIBRARY_PATH=../../../target/debug dotnet run
```
