# munarium-proto

Generated MMP (Munarium Protocol) types and gRPC service stubs, built at compile
time from the normative `.proto` files with a vendored `protoc`. Wire-only: this
crate is the boundary, and nothing else lets these types past a service.

The normative protos live at `server/proto/mmp/v1/` in the
[repository](https://github.com/iokaio/munarium) and are compiled in place. To
publish this crate they must be inside it, because a `.crate` holds only what
is under the crate directory:

    cp -r server/proto server/src/munarium-proto/proto
    cargo package -p munarium-proto
    rm -r server/src/munarium-proto/proto

The copy is ignored by git and must not be committed; `build.rs` prefers it when
present and reads the workspace protos otherwise.

Licensed under Apache-2.0. See LICENSE and NOTICE.
