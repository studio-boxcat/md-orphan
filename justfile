default: build

# Cargo release profile: lto=thin + codegen-units=1 + strip (set in Cargo.toml).
build:
    cargo build --release

# Install into ~/.cargo/bin the cargo way; the repo's target/ is the build cache. Every other
# machine gets the same binary from this repo's git URL (config/setup.sh).
install:
    cargo install --path . --locked --force --target-dir target

test:
    cargo test

run *ARGS:
    cargo run --release -- {{ARGS}}

clean:
    cargo clean

# Compile without emitting — what `boxcat-devenv typecheck` runs for this repo.
typecheck:
    cargo check --all-targets
