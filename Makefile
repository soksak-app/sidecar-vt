# 터미널 engine sidecar repository 의 test, build, release(docs/features.md). release 는 core 의 sok 를 쓴다.
.PHONY: test build release candidate-release repeat push-check hooks

MACOS_MINIMUM = 14.4
PROFILE ?= debug
CARGO_PROFILE = $(if $(filter release,$(PROFILE)),--release,)
FEATURES ?=
CARGO_FEATURE_ARGS = $(if $(strip $(FEATURES)),--features $(FEATURES))
SOK ?= sok

# soksak-events of the @soksak/plugin-api that package.json names applies the census of core to the sources
# (docs/spec/logging.md#census with census.json) and checks the emit sites that the emitters of census.json declare against
# the catalog vt-alacritty/events.json.
SOKSAK_EVENTS ?= pnpm exec soksak-events

ifeq ($(shell uname -s),Darwin)
PROCESS_IDENTITY_PKGCONFIG = $(shell node -e 'const path=require("node:path");const {createRequire}=require("node:module");const r=createRequire(path.join(process.cwd(),"package.json"));process.stdout.write(path.join(path.dirname(r.resolve("@soksak/native-process-identity-darwin/package.json")),"lib/pkgconfig"))')
PROCESS_IDENTITY_ENV = PKG_CONFIG_PATH="$(PROCESS_IDENTITY_PKGCONFIG)$(if $(PKG_CONFIG_PATH),:$(PKG_CONFIG_PATH))"
.PHONY: process-identity-check
process-identity-check: node_modules
	@node -e 'const fs=require("node:fs");const path=require("node:path");const {createRequire}=require("node:module");const r=createRequire(path.join(process.cwd(),"package.json"));const root=path.dirname(r.resolve("@soksak/native-process-identity-darwin/package.json"));for(const f of ["include/soksak_process_identity.h","lib/libsoksak-process-identity.a","lib/pkgconfig/soksak-process-identity.pc"]){if(!fs.existsSync(path.join(root,f)))throw new Error(`native process identity package is missing ${f}`)}'
build: node_modules process-identity-check
endif

node_modules: package.json
	pnpm install
	touch node_modules

# rustfmt, unit tests, the engine trace test of the diagnostics build and clippy without warnings run first; then
# the built service checks connection recovery (scripts/verify-vt-recovery.mjs) and the protocol inventory is
# compared with the engine (scripts/check-terminal-protocol-inventory.mjs).
test: build node_modules
	cargo fmt --all --check
	$(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo test --workspace
	$(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo test -p soksak-sidecar-vt-core --features diagnostics --test engine_trace_test
	$(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo test -p soksak-sidecar-vt-core --features fault-injection --test fault_injection
	$(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo test -p soksak-sidecar-vt-core --features fault-injection --test serve_contract -- a_failed_draw_keeps_the_screen_unsent_so_the_next_frame_names_it --exact
	$(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo clippy --workspace --all-targets --features diagnostics -- -D warnings
	$(SOKSAK_EVENTS) --census census.json --catalog vt-alacritty/events.json .
	node --test tests/
	pnpm exec soksak-records

# `FEATURES=diagnostics` enables the diagnostic service build; the runtime trace remains opt-in.
# sidecar.json 이 가리키는 vt-alacritty/build/soksak-vt-alacritty 를 쓴다.
build:
	$(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo build $(CARGO_PROFILE) $(CARGO_FEATURE_ARGS) -p soksak-sidecar-vt-alacritty
	mkdir -p vt-alacritty/build
	cp target/$(PROFILE)/soksak-vt-alacritty vt-alacritty/build/soksak-vt-alacritty
	cp target/$(PROFILE)/soksak-vt-replay vt-alacritty/build/soksak-vt-replay

release: build
	@test -n "$(OUT)" || { echo "make release OUT=<folder>" >&2; exit 2; }
	$(SOK) sidecar release vt-alacritty $(OUT)

# Build and verify the local diagnostic candidate as one operation. Ordinary release stays non-diagnostic.
candidate-release:
	@test -n "$(OUT)" || { echo "make candidate-release OUT=<folder>" >&2; exit 2; }
	@test "$(shell uname -s)" = Darwin || { echo "candidate-release requires macOS" >&2; exit 2; }
	@mkdir -p "$(OUT)"
	@test -z "$$(find "$(OUT)" -maxdepth 1 -type f -name 'soksak-sidecar-vt-alacritty-*.tar.gz' -print -quit)" || { echo "candidate-release requires an output folder without an existing VT release" >&2; exit 2; }
	$(MAKE) release PROFILE=$(PROFILE) FEATURES=diagnostics OUT="$(OUT)" SOK="$(SOK)"
	@release="$$(find "$(OUT)" -maxdepth 1 -type f -name 'soksak-sidecar-vt-alacritty-*.tar.gz' -print)"; \
	  test -n "$$release" || { echo "candidate-release found no VT release in $(OUT)" >&2; exit 2; }; \
	  test "$$(printf '%s\n' "$$release" | wc -l | tr -d ' ')" = 1 || { echo "candidate-release found multiple VT releases in $(OUT)" >&2; exit 2; }; \
	  SOKSAK_DIAGNOSTIC_PACKAGE_RELEASE="$$release" node --test --test-concurrency=1 tests/versions.test.mjs tests/diagnostic_package.test.mjs

# The check that a push needs: the commit messages of the pushed range, because a published commit message is kept; CI runs
# the tests and checks on the pushed commits.
# RANGE holds the commits of the pushed refs; the pre-push hook sets it, and it defaults to origin/main..HEAD.
push-check:
	pnpm exec soksak-commits "$(or $(RANGE),origin/main..HEAD)"

# Repeats the tests of PACKAGE, or only the test TEST, COUNT times and reports each run with its elapsed time; the
# first failing run prints its output and fails. FEATURES selects the package features used by each run.
repeat:
	@case "$(PACKAGE)" in '') echo "make repeat PACKAGE=<package> COUNT=<n> [TEST=<name>] [FEATURES=<features>]" >&2; exit 2;; esac
	@case "$(COUNT)" in ''|*[!0-9]*|0) echo "make repeat requires COUNT=<n> with n >= 1" >&2; exit 2;; esac
	@$(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo test -q -p $(PACKAGE) $(CARGO_FEATURE_ARGS) --no-run
	@output=$$(mktemp); trap 'rm -f "$$output"' EXIT; run=1; while [ $$run -le $(COUNT) ]; do \
	  started=$$(date +%s); \
	  if ! $(PROCESS_IDENTITY_ENV) MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo test -q -p $(PACKAGE) $(CARGO_FEATURE_ARGS) $(if $(TEST),-- --exact $(TEST)) > "$$output" 2>&1; then \
	    cat "$$output"; echo "FAIL run $$run of $(COUNT) after $$(( $$(date +%s) - started )) s"; exit 1; fi; \
	  echo "PASS run $$run of $(COUNT) in $$(( $$(date +%s) - started )) s"; run=$$((run + 1)); done

# Sets this repository to use .githooks, which checks commit messages and records at commit time.
hooks:
	@git config core.hooksPath .githooks
