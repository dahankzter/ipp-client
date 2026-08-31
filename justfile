default: test

# Runs the unit tests. nextest does not run doctests, so they run separately.
test:
    cargo nextest run --workspace
    cargo nextest run -p cups-client --no-default-features
    cargo test --workspace --doc

# Adds the tests needing a live cupsd.
test-all: test
    cargo nextest run --workspace --run-ignored all

# Everything CI checks, so a push does not discover it for you.
check: test
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
