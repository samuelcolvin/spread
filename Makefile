.DEFAULT_GOAL := help

.PHONY: .cargo
.cargo: ## Check that cargo is installed
	@cargo --version || echo 'Please install cargo: https://github.com/rust-lang/cargo'

.PHONY: install
install: .cargo ## Install the spread binary from this checkout
	cargo install --path . --locked

.PHONY: format
format: .cargo ## Format Rust code with rustfmt
	cargo fmt --all

.PHONY: check
check: .cargo ## Check Rust code with clippy
	cargo clippy --all-targets -- -D warnings

.PHONY: test
test: .cargo ## Run Rust unit tests
	cargo test

# (must stay last!)
.PHONY: help
help: ## Show this help (usage: make help)
	@echo "Usage: make [recipe]"
	@echo "Recipes:"
	@awk '/^[a-zA-Z0-9_-]+:.*?##/ { \
	    helpMessage = match($$0, /## (.*)/); \
	        if (helpMessage) { \
	            recipe = $$1; \
	            sub(/:/, "", recipe); \
	            printf "  \033[36mmake %-20s\033[0m %s\n", recipe, substr($$0, RSTART + 3, RLENGTH); \
	    } \
	}' $(MAKEFILE_LIST)
