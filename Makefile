.PHONY: check fmt fmt-check clippy test doc package-check terminal-check msrv audit

check: fmt-check clippy test doc package-check terminal-check distribution-check

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all --check

clippy:
	cargo clippy --locked --workspace --all-targets -- -D warnings

test:
	cargo test --locked --workspace

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps

package-check:
	python3 scripts/check-package.py

terminal-check:
	cargo build --locked
	python3 scripts/check-terminal.py
	python3 scripts/check-permissions.py
	python3 scripts/check-plans.py
	python3 scripts/check-profiles.py
	python3 scripts/check-processes.py
	python3 scripts/check-shell.py
	python3 scripts/check-herdr.py
	python3 scripts/check-skills.py
	python3 scripts/check-mcp.py
	python3 scripts/check-hooks.py
	python3 scripts/check-instructions.py
	python3 scripts/check-docs.py

msrv:
	cargo +1.94.0 test --locked --workspace

audit:
	cargo deny $(DENY_FLAGS) check

# Optional Linux integration; owns a private display, never the user's clipboard.
.PHONY: clipboard-check
clipboard-check:
	cargo build --locked
	python3 scripts/check-clipboard.py

.PHONY: distribution-check
distribution-check:
	ROOMKIT_RELEASE_BUILD=1 cargo build --locked
	python3 scripts/check-distribution.py
