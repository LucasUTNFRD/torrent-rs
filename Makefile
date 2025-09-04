.PHONY: build lint test clean run

build: 
	cargo build --workspace

lint: 
	cargo clippy --all-targets --all-features --workspace -- -D warnings

test: 
	cargo test --workspace

clean: 
	cargo clean

