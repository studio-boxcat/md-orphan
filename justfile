default: build

# Cargo release profile: lto=thin + codegen-units=1 + strip (set in Cargo.toml).
build:
    cargo build --release
    cp target/release/md-orphan dist/md-orphan

install: build
    ln -sf {{justfile_directory()}}/dist/md-orphan ~/.local/bin/md-orphan

test:
    cargo test

run *ARGS:
    cargo run --release -- {{ARGS}}

clean:
    cargo clean
