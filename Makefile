.PHONY: build lint test clean run

build: 
	cargo build --release

lint: 
	cargo clippy --all-targets  --workspace -- -D warnings

test:  # run unit test
	cargo test --workspace


clean: 
	cargo clean

