# rorm-lib

![license](https://img.shields.io/github/license/rorm-orm/rorm-lib?label=License)
![crates-io-version](https://img.shields.io/crates/v/rorm-lib)
![docs](https://img.shields.io/docsrs/rorm-lib?label=Docs)

`rorm-lib` provides FFI bindings for [rorm-db](https://github.com/rorm-orm/rorm-db).

With the help of this crate, it is possible for other languages to use the orm.

To provide a similar experience as `rorm` for users, it is required to build 
an additional layer upon this binding as well as an interface for `rorm-cli`.
