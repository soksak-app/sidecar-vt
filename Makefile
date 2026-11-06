# Test, build, and release of the terminal engine sidecar repository (docs/features.md).
# The release is the npm pack file of vt-alacritty.
.PHONY: install test build release check-cargo release-candidate repeat check-push install-hooks

# rustup installs cargo in $(CARGO_HOME)/bin, and in ~/.cargo/bin when CARGO_HOME is unset; a GitHub runner sets CARGO_HOME
# elsewhere (S165). rustup adds that folder to the shell profile, which the shell of make does not read (S164).
CARGO_HOME ?= $(HOME)/.cargo
export PATH := $(CARGO_HOME)/bin:$(PATH)
# GNU Make 3.81 runs a recipe line without shell characters itself and looks its program up in its own PATH, not in the
# exported one, and $(shell) does not receive the exported PATH either; so cargo is resolved once with that PATH and every
# recipe runs $(CARGO). check-cargo stops a target that runs cargo when none was found (S164).
CARGO := $(shell PATH="$(PATH)" command -v cargo)

# The oldest macOS that the sidecar runs on, the value of core (core Makefile MACOS_MINIMUM). Every command that runs Cargo,
# and the node tests that start Cargo, give it as MACOSX_DEPLOYMENT_TARGET, which cc reads for the Objective-C objects and
# rustc reads for the link (S163).
MACOS_MINIMUM = 14.4
# Each command that runs Cargo runs under scripts/check-link.mjs, which fails it when the linker linked an object built for a
# newer macOS than MACOS_MINIMUM and passes the other linker lines; cargo-nextest runs under scripts/nextest-run.mjs, which
# applies the same check (S163).
CHECK_LINK = node scripts/check-link.mjs
PROFILE ?= debug
# The folder where Cargo writes; build copies the executables from it. A value from the environment or the command line reaches Cargo too.
CARGO_TARGET_DIR ?= target
CARGO_PROFILE = $(if $(filter release,$(PROFILE)),--release,)
FEATURES ?=
CARGO_FEATURE_ARGS = $(if $(strip $(FEATURES)),--features $(FEATURES))
# The platform key of the release (core docs/spec/installation.md#sidecar-release).
PLATFORM := $(shell node -p '(process.platform === "win32" ? "windows" : process.platform) + "-" + process.arch')

# sok-check events of the kit release that package.json pins applies the event source rules to the sources
# (docs/spec/logging.md#event-sources with event-sources.json) and checks the emit sites that the emitters of event-sources.json declare against
# the catalog vt-alacritty/events.json.

ifeq ($(shell uname -s),Darwin)
PROCESS_IDENTITY_PKGCONFIG = $(shell node -e 'const path=require("node:path");const {createRequire}=require("node:module");const r=createRequire(path.join(process.cwd(),"package.json"));process.stdout.write(path.join(path.dirname(r.resolve("@soksak/native-process-identity-darwin/package.json")),"lib/pkgconfig"))')
PROCESS_IDENTITY_ENV = PKG_CONFIG_PATH="$(PROCESS_IDENTITY_PKGCONFIG)$(if $(PKG_CONFIG_PATH),:$(PKG_CONFIG_PATH))"
.PHONY: check-process-identity
check-process-identity:
	@node -e 'const fs=require("node:fs");const path=require("node:path");const {createRequire}=require("node:module");const r=createRequire(path.join(process.cwd(),"package.json"));const root=path.dirname(r.resolve("@soksak/native-process-identity-darwin/package.json"));for(const f of ["include/soksak_process_identity.h","lib/libsoksak-process-identity.a","lib/pkgconfig/soksak-process-identity.pc"]){if(!fs.existsSync(path.join(root,f)))throw new Error(`native process identity package is missing ${f}`)}'
build: check-process-identity
endif

check-cargo:
	@test -n "$(CARGO)" || { echo "make: cargo is on neither PATH nor $(CARGO_HOME)/bin; rustup installs it there" >&2; exit 1; }
build test repeat: check-cargo

# The installation stage (core docs/spec/plugins.md#repositories): it installs the locked dependencies; the tests, checks and packs consume them without installing.
install:
	pnpm install --frozen-lockfile

# rustfmt, unit tests, the engine trace test of the diagnostics build and clippy without warnings run first; then
# the built service checks connection recovery (scripts/verify-vt-recovery.mjs) and the protocol inventory is
# compared with the engine (scripts/check-terminal-protocol-inventory.mjs).
# Each Rust test runs under cargo-nextest through scripts/nextest-run.mjs, which prints a running line before each result;
# cargo-nextest reports the elapsed time and applies the limit of each test from .config/nextest.toml (S76, S76-1).
# make test runs no doc test: cargo-nextest does not run them and cargo test --doc has no per-test time or limit, so
# the usage of a Rust type is a unit test (S76-2).
NEXTEST = $(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) CARGO_INCREMENTAL=0 node scripts/nextest-run.mjs --config-file .config/nextest.toml --no-fail-fast
test: build
	$(CARGO) fmt --all --check
	$(NEXTEST) --workspace
	$(NEXTEST) -p soksak-sidecar-vt-core --features diagnostics --test engine_trace_test
	$(NEXTEST) -p soksak-sidecar-vt-core --features fault-injection --test fault_injection
	$(NEXTEST) -p soksak-sidecar-vt-core --features fault-injection --test serve_contract -E 'test(=a_failed_draw_keeps_the_screen_unsent_so_the_next_frame_names_it)'
	$(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) $(CHECK_LINK) $(CARGO) clippy --workspace --all-targets --features diagnostics -- -D warnings
	pnpm exec sok-check events --config event-sources.json --catalog vt-alacritty/events.json .
	MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) node --test tests/
	pnpm exec sok-check docs
	pnpm exec sok-check commands

# `FEATURES=diagnostics` enables the diagnostic service build; the runtime trace remains opt-in.
# Uses vt-alacritty/build/sok-vt-alacritty, which sidecar.json points to.
build:
	$(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) $(CHECK_LINK) $(CARGO) build $(CARGO_PROFILE) $(CARGO_FEATURE_ARGS) -p soksak-sidecar-vt-alacritty
	mkdir -p vt-alacritty/build
	cp $(CARGO_TARGET_DIR)/$(PROFILE)/sok-vt-alacritty vt-alacritty/build/sok-vt-alacritty
	cp $(CARGO_TARGET_DIR)/$(PROFILE)/sok-vt-replay vt-alacritty/build/sok-vt-replay

release: build
	@test -n "$(OUT)" || { echo "make release OUT=<folder>" >&2; exit 2; }
	@mkdir -p $(OUT)
	@# npm pack keeps the execute bit of the executables, which pnpm pack drops.
	file="$$(npm pack --silent --pack-destination $(OUT) ./vt-alacritty)" && mv "$(OUT)/$$file" "$(OUT)/$${file%.tgz}-$(PLATFORM).tgz"

# Build and verify the local diagnostic candidate as one operation. Ordinary release stays non-diagnostic.
release-candidate:
	@test -n "$(OUT)" || { echo "make release-candidate OUT=<folder>" >&2; exit 2; }
	@test "$(shell uname -s)" = Darwin || { echo "release-candidate requires macOS" >&2; exit 2; }
	@mkdir -p "$(OUT)"
	@test -z "$$(find "$(OUT)" -maxdepth 1 -type f -name 'soksak-sidecar-vt-alacritty-*.tgz' -print -quit)" || { echo "release-candidate requires an output folder without an existing VT release" >&2; exit 2; }
	$(MAKE) release PROFILE=$(PROFILE) FEATURES=diagnostics OUT="$(OUT)"
	@release="$$(find "$(OUT)" -maxdepth 1 -type f -name 'soksak-sidecar-vt-alacritty-*.tgz' -print)"; \
	  test -n "$$release" || { echo "release-candidate found no VT release in $(OUT)" >&2; exit 2; }; \
	  test "$$(printf '%s\n' "$$release" | wc -l | tr -d ' ')" = 1 || { echo "release-candidate found multiple VT releases in $(OUT)" >&2; exit 2; }; \
	  SOKSAK_DIAGNOSTIC_PACKAGE_RELEASE="$$release" node --test --test-concurrency=1 tests/versions.test.mjs tests/diagnostic_package.test.mjs

# The checks that a push needs: the commit messages and the checklist history of the pushed range, because a published
# commit is kept; CI runs the tests and the other checks on the pushed commits.
# RANGE holds the commits of the pushed refs; the pre-push hook sets it, and it defaults to origin/main..HEAD.
check-push:
	pnpm exec sok-check commits "$(or $(RANGE),origin/main..HEAD)"
	pnpm exec sok-check docs --range "$(or $(RANGE),origin/main..HEAD)"

# Repeats the tests of PACKAGE, or only the test TEST, COUNT times and reports each run with its elapsed time; the
# first failing run prints its output and fails. FEATURES selects the package features used by each run.
repeat:
	@case "$(PACKAGE)" in '') echo "make repeat PACKAGE=<package> COUNT=<n> [TEST=<name>] [FEATURES=<features>]" >&2; exit 2;; esac
	@case "$(COUNT)" in ''|*[!0-9]*|0) echo "make repeat requires COUNT=<n> with n >= 1" >&2; exit 2;; esac
	@$(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) $(CHECK_LINK) $(CARGO) test -q -p $(PACKAGE) $(CARGO_FEATURE_ARGS) --no-run
	@output=$$(mktemp); trap 'rm -f "$$output"' EXIT; run=1; while [ $$run -le $(COUNT) ]; do \
	  started=$$(date +%s); \
	  if ! $(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) $(CHECK_LINK) $(CARGO) test -q -p $(PACKAGE) $(CARGO_FEATURE_ARGS) $(if $(TEST),-- --exact $(TEST)) > "$$output" 2>&1; then \
	    cat "$$output"; echo "FAIL run $$run of $(COUNT) after $$(( $$(date +%s) - started )) s"; exit 1; fi; \
	  echo "PASS run $$run of $(COUNT) in $$(( $$(date +%s) - started )) s"; run=$$((run + 1)); done

# Sets this repository to use .githooks, which checks commit messages and records at commit time.
install-hooks:
	@git config core.hooksPath .githooks
