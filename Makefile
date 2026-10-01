# 터미널 engine sidecar repository 의 test, build, release(docs/features.md). release 는 core 의 sok 를 쓴다.
.PHONY: test build release

MACOS_MINIMUM = 14.4
PROFILE ?= debug
CARGO_PROFILE = $(if $(filter release,$(PROFILE)),--release,)
SOK ?= sok

test:
	MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo test --workspace

# sidecar.json 이 가리키는 vt-alacritty/build/soksak-vt-alacritty 를 쓴다.
build:
	MACOSX_DEPLOYMENT_TARGET=$(MACOS_MINIMUM) cargo build $(CARGO_PROFILE) -p soksak-sidecar-vt-alacritty
	mkdir -p vt-alacritty/build
	cp target/$(PROFILE)/soksak-vt-alacritty vt-alacritty/build/soksak-vt-alacritty

release: build
	@test -n "$(OUT)" || { echo "make release OUT=<folder>" >&2; exit 2; }
	$(SOK) sidecar release vt-alacritty $(OUT)
