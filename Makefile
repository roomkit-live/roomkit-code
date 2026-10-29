.PHONY: check fmt fmt-check clippy test doc package-check terminal-check msrv audit

check: fmt-check clippy test doc package-check terminal-check

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all --check

clippy:
	cargo clippy --locked --all-targets -- -D warnings

test:
	cargo test --locked

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps

package-check:
	python3 scripts/check-package.py

terminal-check:
	cargo build --locked
	python3 scripts/check-terminal.py
	python3 scripts/check-permissions.py

msrv:
	cargo +1.92.0 test --locked

audit:
	cargo deny $(DENY_FLAGS) check
