default: check

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --all-targets -- -D warnings

build:
    cargo build

release:
    cargo build --release

test:
    cargo test

check: fmt-check lint build test

run *args:
    cargo run -- {{args}}

clean:
    cargo clean
