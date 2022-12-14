# rorm-lib

![license](https://img.shields.io/github/license/rorm-orm/rorm-lib?label=License)
[![crates-io-version](https://img.shields.io/crates/v/rorm-lib)](https://crates.io/crates/rorm-lib)
[![docs](https://img.shields.io/docsrs/rorm-lib?label=Docs)](https://docs.rs/rorm-lib/latest/rorm/)

`rorm-lib` provides FFI bindings for [rorm-db](https://github.com/rorm-orm/rorm-db).

With the help of this crate, it is possible for other languages to use the orm.

## Compile `rorm-lib`

In order to compile `rorm-lib`, a [rustup](https://rustup.rs/) installation 
is recommended.

```bash
## Clone the repository
git clone --recursive -b main https://github.com/rorm-orm/rorm-lib && cd rorm-lib
## To compile the current development version, use:
# git clone --recursive -b dev https://github.com/rorm-orm/rorm-lib && cd rorm-lib

# Compile the release version (optimized + no debug symbols) 
cargo build -r -p rorm-lib
## or build the debug build:
# cargo build -p rorm-lib
## or also enable the logging functionality:
# cargo build -p rorm-lib -F logging
```

The resulting libraries will be written to `./target/release/` or `./target/debug/`
depending on the build type.

## API definition

The current API definition can be generated by using `cbindgen`:

```bash
# Install / update cbindgen
cargo install -f cbindgen

# Generate header
cbindgen --crate rorm-lib --config cbindgen.toml --output rorm.h
```

The generated header file is located in `./rorm.h`.

## Further notes

To provide a similar experience as `rorm` for users, it is required to build 
an additional layer upon this binding as well as an interface for `rorm-cli`.
