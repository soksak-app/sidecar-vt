# 터미널 engine sidecar repository 의 test, build, release(docs/features.md). release 는 core 의 sok 를 쓴다.
.PHONY: test build release repeat

MACOS_MINIMUM = 14.4
PROFILE ?= debug
CARGO_PROFILE = $(if $(filter release,$(PROFILE)),--release,)
SOK ?= sok

# soksak-events of the @soksak/plugin-api that package.json names applies the census of core to the sources
# (docs/spec/logging.md#census with census.json) and checks the emit sites that the emitters of census.json declare against
# the catalog vt-alacritty/events.json.
SOKSAK_EVENTS ?= pnpm exec soksak-events

node_modules: package.json
	pnpm install
	touch node_modules

# rustfmt, unit tests, the engine trace test of the diagnostics build and clippy without warnings run first; then
# the built service checks connection recovery (scripts/verify-vt-recovery.mjs) and the protocol inventory is
# compared with the engine (scripts/check-terminal-protocol-inventory.mjs).
test: build node_modules
	cargo fmt --all --check
	MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo test --workspace
	MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo test -p soksak-sidecar-vt-core --features diagnostics --test engine_trace_test
	MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo clippy --workspace --all-targets --features diagnostics -- -D warnings
	$(SOKSAK_EVENTS) --census census.json --catalog vt-alacritty/events.json .
	node --test tests/
	pnpm exec soksak-records

# sidecar.json 이 가리키는 vt-alacritty/build/soksak-vt-alacritty 를 쓴다.
build:
	MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo build $(CARGO_PROFILE) -p soksak-sidecar-vt-alacritty
	mkdir -p vt-alacritty/build
	cp target/$(PROFILE)/soksak-vt-alacritty vt-alacritty/build/soksak-vt-alacritty
	cp target/$(PROFILE)/soksak-vt-replay vt-alacritty/build/soksak-vt-replay

release: build
	@test -n "$(OUT)" || { echo "make release OUT=<folder>" >&2; exit 2; }
	$(SOK) sidecar release vt-alacritty $(OUT)

# Repeats the tests of PACKAGE, or only the test TEST, COUNT times and reports each run with its elapsed time; the
# first failing run prints its output and fails. Intermittent failures are reproduced and accepted with this target.
repeat:
	@case "$(PACKAGE)" in '') echo "make repeat PACKAGE=<package> COUNT=<n> [TEST=<name>]" >&2; exit 2;; esac
	@case "$(COUNT)" in ''|*[!0-9]*|0) echo "make repeat requires COUNT=<n> with n >= 1" >&2; exit 2;; esac
	@MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo test -q -p $(PACKAGE) --no-run
	@output=$$(mktemp); trap 'rm -f "$$output"' EXIT; run=1; while [ $$run -le $(COUNT) ]; do \
	  started=$$(date +%s); \
	  if ! MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo test -q -p $(PACKAGE) $(if $(TEST),-- --exact $(TEST)) > "$$output" 2>&1; then \
	    cat "$$output"; echo "FAIL run $$run of $(COUNT) after $$(( $$(date +%s) - started )) s"; exit 1; fi; \
	  echo "PASS run $$run of $(COUNT) in $$(( $$(date +%s) - started )) s"; run=$$((run + 1)); done
