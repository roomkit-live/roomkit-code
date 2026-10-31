.PHONY: check fmt fmt-check clippy test doc package-check terminal-check msrv audit
.PHONY: build debug clean install uninstall

# Managed installation directory, shared with the release installer.
BIN_DIR ?= $(HOME)/.local/bin
TARGET_DIR := $(or $(CARGO_TARGET_DIR),target)

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
	python3 scripts/check-multi-session.py
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
	python3 scripts/check-exit.py
	python3 scripts/check-files.py
	python3 scripts/check-context.py

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

# Production binary: release profile, with the distribution marker so that a
# managed installation keeps update checks and rollback.
build:
	ROOMKIT_RELEASE_BUILD=1 cargo build --locked --release

# Development binary in target/debug, without the distribution marker.
debug:
	cargo build --locked

clean:
	cargo clean

# Activate the production binary in BIN_DIR as the release installer does.
# An unrelated roomkit-code there is never replaced.
install: build
	"$(TARGET_DIR)/release/roomkit-code" --install-dir "$(BIN_DIR)"

# Remove only a managed installation; application data and settings are kept.
uninstall:
	@entry="$(BIN_DIR)/roomkit-code"; \
	if [ "$$(readlink "$$entry" 2>/dev/null)" = ".roomkit-code/current/roomkit-code" ]; then \
		rm -f "$$entry" && rm -rf "$(BIN_DIR)/.roomkit-code" && \
		echo "Removed RoomKit Code from $(BIN_DIR); application data is kept."; \
	elif [ -e "$$entry" ] || [ -L "$$entry" ]; then \
		echo "$$entry is not a managed installation; left unchanged." >&2; exit 1; \
	else \
		echo "No RoomKit Code installation in $(BIN_DIR)."; \
	fi
