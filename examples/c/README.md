## Calculator

1. Ensure `libcalculator.dylib` is built in `target/debug`.
2. Build the example:

```
cc -I ../../generated/c main.c -L ../../target/debug -lcalculator -o c_example
```

3. Run:

```
DYLD_LIBRARY_PATH=../../target/debug ./c_example
```

## Contacts

1. Build the contacts library (from repo root):

```
cargo build -p contacts
```

2. Generate the C header:

```
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated
```

3. Build the contacts example:

```
cc -I ../../generated/c contacts.c -L ../../target/debug -lcontacts -o contacts_example
```

4. Run:

```
# macOS
DYLD_LIBRARY_PATH=../../target/debug ./contacts_example

# Linux
LD_LIBRARY_PATH=../../target/debug ./contacts_example
```
