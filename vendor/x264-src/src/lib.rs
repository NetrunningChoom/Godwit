//! x264's H264 encoder as a static library, for C code to call through
//! `x264.h`: the build script compiles it and links it, and hands the
//! folder holding `x264.h` to dependents' build scripts as
//! `DEP_X264_INCLUDE`. Rust code reaches it only through those shims.
