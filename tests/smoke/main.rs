// Real-network smoke tests for the toolchains maintained by this fork.
//
// To run these locally:
// ```
// VOLTA_LOGLEVEL=debug cargo test --test smoke --features smoke-tests -- --test-threads 1
// ```
//
// The tests share one temporary project path and must run serially. HOME and VOLTA_HOME are
// isolated from the developer's normal Volta installation.

cfg_if::cfg_if! {
    if #[cfg(all(unix, feature = "smoke-tests"))] {
        mod maintained_toolchains;
        pub mod support;
    }
}
