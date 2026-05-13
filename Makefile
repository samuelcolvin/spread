.DEFAULT_GOAL := help

APP_NAME ?= Spread
APP_DIR ?= $(HOME)/Applications/$(APP_NAME).app
BUNDLE_ID ?= com.samuelcolvin.spread
LSREGISTER ?= /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

.PHONY: .cargo
.cargo: ## Check that cargo is installed
	@cargo --version || echo 'Please install cargo: https://github.com/rust-lang/cargo'

.PHONY: install
install: .cargo ## Install dependencies and set up pre-commit hooks
	cargo build
	uvx prek install --install-hooks

.PHONY: install-app
install-app: .cargo macos-app ## Install the spread CLI and macOS app bundle
	cargo install --path . --locked

.PHONY: macos-app
macos-app: .cargo ## Build and install Spread.app for Finder integration
	cargo build --release --locked
	install -d "$(APP_DIR)/Contents/MacOS" "$(APP_DIR)/Contents/Resources"
	install -m 755 target/release/spread "$(APP_DIR)/Contents/MacOS/spread"
	install -m 644 packaging/macos/Info.plist "$(APP_DIR)/Contents/Info.plist"
	@echo "Installed $(APP_DIR)"
	@echo "Registering Spread.app with macOS Launch Services"
	"$(LSREGISTER)" -f "$(APP_DIR)"

.PHONY: format
format: .cargo ## Format Rust code with rustfmt
	cargo fmt --all

.PHONY: check
check: .cargo ## Check Rust code with clippy
	cargo clippy --all-targets -- -D warnings

.PHONY: lint
lint: check ## Alias for make check

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
