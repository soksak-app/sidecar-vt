# Changelog

- S131-2: One I/O task reads and writes a session: reads stop at 16 chunks of 64 KiB, a write never waits in the surface task, and the surface acknowledges a write from its result.
- S131-1: The session holds no ring and no attach path, so output that the consumer read leaves no copy; a connection that is not persistent closes its sessions when it ends.
- S130: The cost of a frame on 0.0.23 is measured: a status line 1.7 ms per frame and 20 rows 3.0 ms per frame.
- S129: Version 0.0.23, which carries S122 to S128.
- S110: Status 5 of the service start is the readback result of the process identity library, and the pinned library 0.0.25 names the value it found.
- S128: The sidecar uses the records kit 0.0.6, whose release gate accepts a closed item with a `Closed:` reason.
- S127: The image surface of a session redraws only the strips of changed rows.
- S126: The engine builds a screen without an allocation per cell.
- S125: The service sends no screen message for a frame, answers `screen.read` with the full form and sends a `view` event when the cursor or the scrollback of a frame changes.
- S124: A frame whose record the event log does not store is not hashed.
- S123: A test measures that output which the frame clock holds builds no screen.
- S122: Output is drawn on a frame clock of 33 ms, output within 200 ms after an input is drawn at once, and output that the clock holds builds no screen.
- S120: Version 0.0.22, which carries S119.
- S119: While the host reports that its image region is not drawing, a surface draws no frame and sends no `screen` event; when the region draws again it draws and sends the changed screen once. A drawing state of an earlier generation is ignored, and an invalid drawing message is answered with `invalidParams`.
- S118: Version 0.0.21, which carries S117.
- S117: Sessions that start at the same time each write the shell integration files through a temporary file of their own, so opening several terminals at once no longer fails with `install shell integration file ...: No such file or directory`. Red: the concurrent install test failed with that error. Green: it passes.
- S115: Version 0.0.20, which carries S114.
- S116: The diagnostic package test applies each screen message to the screen of the message it follows, so a change form without `cells` no longer fails the candidate release. Red: the candidate release failed with `screen cells are required`. Green: it passes.
- S114: Every `screen` message carries `number`, and a message carries only the rows that changed since the sent message that it names (`base: false`, `previous`, `changed`) except the full form after a resize, after a reconnection, after an unsent message, for `screen.read` and when more than half of the rows changed; the line is written from the held rows without a copy of the cells.
- F355-4-vt: Version 0.0.18. `make release OUT=<folder>` writes the release with `npm pack` of `vt-alacritty`, which keeps the execute bit, and release.yml no longer downloads the sok of a core release, because core installs the `npm pack` file of a sidecar and has no `sok sidecar release`. The dependencies on core are pinned to the core release `v0.0.25`.
- F272-vt-sidecar: A push to `main` releases the declared version through `soksak-release-gate` when its tag is absent, with the `sok` of the core release `v0.0.21`, and the dependencies on core are the packed release files of that release; no workflow checks out core.
- S111-1: The service records each request still in progress 10 seconds after it arrived as the error event `sidecar.vt.request_waiting` with its operation, surface and the stage it waits in, so a stop of the input loop or of a surface actor names the waiting request.
- S109: After an owner close or a retain, a message of a surface whose session ended is handled as the message of a new surface instead of being refused as a stale attachment, because the connection clears the sender and the attachment epoch of that surface.
- S108: A `scroll` operation may carry an optional boolean `shift`; with Shift held, the wheel skips mouse reporting and moves the viewport or sends the alternate scroll keys. An absent `shift` keeps the scroll without Shift, so pages that send no `shift` keep working, and a non-boolean `shift` is refused with `invalidParams`.
- S107: The service records each change of the mouse modes as `sidecar.vt.mouse_modes` and each scroll operation as `sidecar.vt.wheel` with its route and the modes in force, so the log shows whether the wheel reached the program or the viewport.
- S45: The refusal `selection presentation is still pending` is absent from v0.0.9 and later, which carry the S43 correction; the item closes without a code change.
- S18: `ci.yml` runs the tests of this repository on a macOS runner, and `release.yml` published the sidecar archive for each pushed tag from v0.0.3 through v0.0.11.
- S106: A detach refused as a stale attachment or as malformed, whose request is a nonempty string, is answered as `body.image.detached` with its request, name, generation and error, so the host no longer waits for the 10 second timeout.
- S105: The comments of the tracked sources are written in English, and `make test` fails a comment in another language.
- F275-vt-sidecar: `make install` installs the locked dependencies; the tests, checks and builds no longer install.
- F274-1-vt-sidecar: The make target test prints a recipe without the directory lines of a nested make.
- S104: The recovery verifier sends the input transaction of `actions` and `preedit`.
- S103: The core SDK, the process identity package and `CORE_COMMIT` are pinned to the core commit whose census accepts a reporting lock recovery.
- S102: Each poisoned-lock recovery of `vt-core/src/pty.rs` takes the guard with `into_inner` at its call site, so the core census accepts it.
- F274-vt-sidecar: The pre-push hook checks only the commit messages that a push publishes; CI runs the tests and checks.
- S101: The vt-core tests send the input transaction, read error text from `reason`, keep marked ranges within the preedit and read the JSON Schema catalog, so `make test` passes; no production code changed.

- S100: The replay tests send the input transaction and answer each drawn image with its generation and raster, so the service draws the frames that the tests wait for; the repeat target passes 2 of 2.

- S86-2: Verify the bounded event writer in a locally installed diagnostic package with terminal 0.0.21. Continuous ASCII input exposes the original package failure and passes in the corrected package; all 50 accepted records remain ordered after shutdown. Failed package checks retain diagnostic evidence. No public release is included.

- S87: A queued flush now acknowledges its own FIFO boundary without consuming the next request. A controlled owner fails on the discarded second acknowledgement before correction and passes afterward; all 14 event-contract cases pass. Application delivery remains pending.

[한국어](CHANGELOG.ko.md)

## Unreleased

- S113: The version is 0.0.19.
- F355-3-4-vt: The checks come from the records kit and events kit releases instead of the plugin API of core, and `SOKSAK_EVENTS`, which ran the census check of a core checkout, is removed. Red: `the checks come from the kit releases, and no check takes a command or the plugin API of core` failed. Green: it passes, and the workflow, events and census tests pass.
- S112: `pnpm-workspace.yaml` no longer allows the build script of a git source of core, because the dependency is the packed release file of core v0.0.21, which has no install script.
- S97.1, S75.2 and S86-3: v0.0.17 is published and the registry lists it.
- S76-2: `make test` runs no doc test. The doc example of `CompositionState` repeated the unit test `test_composition_state_not_composing`, which runs through cargo-nextest with its own elapsed time and 60-second limit, so the example became prose. Before the change, the doc test finished without a per-test time; after it, 488 Rust tests each print RUNNING and a PASS line with elapsed time, and `make test` exits 0.

- S76-1: `make test` prints a RUNNING line before the result of each Rust test. `scripts/nextest-run.mjs` runs cargo-nextest with its libtest JSON output, which emits a started event per test, because the human output of nextest 0.9.146 has no start line. Before the change, 488 Rust test results had no RUNNING line; after it, `make test` prints 488 RUNNING lines, each followed by its PASS line, and exits 0.

- S76: `make test` runs the Rust tests through cargo-nextest 0.9.146 with `.config/nextest.toml`, which reports each test's result and elapsed time and terminates a test after 60 seconds; no suite-wide limit is set. The final run passes 488 Rust test runs, 29 of 30 node tests, with 1 skipped, and the records check. The doc test still runs through `cargo test --doc` without a per-test time (S76-2), and nextest prints no start line for a test that finishes within 10 seconds (S76-1). CI installs cargo-nextest with `taiki-e/install-action`.

- S99: Allow the pinned Darwin process-identity Git dependency to run its pnpm 11.24.0 `prepare` script by exact repository identity. The clean-consumer owner fails without approval despite exit 0, then passes with a fresh isolated store and confirms the manifest, header, static library and pkg-config file after `prepare: make`. No core package or release change is included.

- S98: Align the plugin SDK, Darwin process-identity dependency and release build checkout to the public core commit that declares 0.0.21. A clean pnpm 11.24.0 lock-only resolution records both packages at 0.0.21, and the installed-package consumer passes with the existing developer link. A clean frozen install exits 0 but leaves the optional native package unresolved (`MODULE_NOT_FOUND`); S73-1 remains open. No release is included.

- S97: Map AppKit `cancelOperation:` to terminal Escape so committed Hangul and Escape reach the PTY in one ordered write. The owning service test passes for the combined transaction, cancel alone, ordinary Escape, final preedit clearing, and atomic rejection of unknown selectors. The selector-mapping owner passes. No application, activation, full suite, or public release is included.

- S93: Move an owned normalized screen body into its surface envelope and serialize with serde_json instead of cloning the cell tree through json!. Both raster and plain senders use the documented encoder. The 8,192-cell regression fails at 57,372 allocations before correction and passes the 64-allocation ceiling afterward, preserving all values and escape bytes; all four affected owners pass. The untraced both-host benchmarks retain complete reports and still fail performance targets. Restore the diagnostic installations and keep performance acceptance open.
- S94: Move the original screen into blocking message preparation after raster drawing, removing the task-closure deep copy while retaining the render-layout copy and screen message contents. Red: the owning 8,192-cell preparation case fails at 8,260 allocations against its 64-allocation transfer bound. Green: the same case passes at 2 allocations and validates surface, frame, dimensions and all 8,192 cells. The three preparation owners, all 10 screen-message owners, frame-record coherence, image-envelope output, page screen-wire and inline-image raster owners pass. The full workspace format check still reports formatting differences in the unchanged file in `vt-core/tests/pty/lifecycle_test.rs`; the changed test file was formatted. End-to-end performance acceptance remains open.
- S95: Resolve the four diagnostics library clippy findings and format the flagged lifecycle test file. Return the checked viewport conversion directly, use `Vec::new` for row initialization, box `SendError<Dispatch>` without discarding its dispatch payload, and collapse the session-matched daemon error send into its match guard. Diagnostics library clippy passes; the closed-surface owner passes in 0.142s; rustfmt passes for the touched Rust files. Full suites and runtime checks are not included.
- S96: Move the owned normalized cell array, frame, render and cursor values directly into the screen body instead of rebuilding their Value trees through `json!`. The owning 8,192-cell Red measures 73,730 allocations for `message_cells` and 131,127 for the unchanged full body, a 57,397-allocation copy. Green passes the same owner within the fixed 128-allocation metadata allowance and verifies all message fields, cells and the unchanged screen hash. Screen-message golden, frame-record correlation, consumer screen-wire and diagnostics library clippy owners pass. No suite, runtime or end-to-end performance check is claimed.

- S92: Separate image-response channel/actor queue time from total consumed wait in the existing event. Use one handling timestamp and sample the ingress clock only while frame debug logging is enabled; a missing ingress timestamp remains null. The missing-field owner fails before correction and passes afterward; catalog owners and both local geometry cases pass. All 160 current answers have queue timing. Preserve scheduling and frame ownership, restore both test installations, and keep performance acceptance open.

- S73-2: Verify the installed Darwin native process-identity package through public package resolution and its build files. The owning consumer passes and the standard sidecar build succeeds on macOS arm64. Replace the remote-install fixture that omitted local overrides; keep local workspace/lock overrides uncommitted and external Git-ref installation acceptance open under S73-1.

- F239-77-vt-sidecar: Complete local schema-3 catalog acceptance with the matching installed diagnostic cohort. The current SDK checks 34 sources with zero pending rules and 32 events; both host installations contain the exact source catalog and pass application log audits. Public publication remains separate.

- S88-2-1: Deliver the verified recovery corrections in a locally installed Darwin diagnostic package. The package owner passes with metadata, engine trace and continuous logging; both application hosts preserve the shared service and reap all terminal children after completed retention. Source fixtures prove injected poison recovery. No public release or IME acceptance is included.

- S88-7: Recover the remaining PTY session field guards and poisoned child condition waits without reconstructing stored values. Ten controlled Red cases become Green and eight existing Rust owners pass. Both current-core catalog/census owners pass; the VT source audit S88 closes with zero findings, pending rules and marker errors, while local application delivery remains pending.

- S88-4-1: Place the remaining feature-gated PTY wait-fault fixture in its owning test directory with its public module and behavior unchanged. The existing one-shot wait-failure owner passes. Two test-only source matches leave production classification; 20 production matches remain. No census criteria or exclusions change.

- S88-6: Recover PTY output and attachment locks without replacing retained data or receivers. Record each field recovery once with its session identity. Three controlled Red cases become Green and three existing replay/buffer owners pass. The source census decreases from 29 to 22 findings; local diagnostic delivery remains pending.

- S88-5: Recover the image transfer store after poisoning with its connection event writer. Retain frame ownership and ended state, report recovery once and keep exact-answer release. Three controlled Red cases become Green and four existing connection/image owners pass. The source census decreases from 30 to 29 findings, all in PTY. Installed application delivery remains pending.

- S88-4: Place existing watchdog, PTY lifecycle and feature-gated event fault fixtures under their owning test directories. Preserve executable test code, feature availability, public helper methods and test names. Seven owning cases pass. Eleven test-only census matches leave production classification without changing criteria or exclusions; 30 production lock findings remain. This is test placement and classification, not a runtime correction.

- S88-3: Recover the PTY session registry at its six production access sites, retain the shared map and record each poisoning once. Three controlled Red cases become Green, and real PTY close/exit and reattach owners pass. Unknown-session errors remain unchanged. The source census decreases from 47 to 41 findings; local package delivery remains pending.

- S88-2: Recover the engine event queue after poisoning, retain FIFO events and report one internal runtime error through the existing structured error writer. Separate internal runtime failures from program sequence rejections, including headless operation. Four controlled owners and three existing event owners pass. The source census decreases from 55 to 47 findings. Installed application delivery remains pending.

- S88-1: Document the valid optional caret, viewport, blank-row and diagnostic metadata defaults and replace the nonempty extent maximum fallback with a caret-seeded fold. Both frame owners pass. The current-core census decreases from 60 to 55 findings, all remaining findings concerning poisoned locks; the catalog accepts all 32 events. This is a conformance audit, not a new display fix.

- S91: Make the pending-image fixture answer its actual first frame instead of an unrelated stale identity. The old fixture fails unchanged source and contradicts the exact-frame contract. The corrected owner passes, observes the entire existing no-image interval and validates the next frame identity without changing production code or time limits.

- S90: Prepare raster-associated screen JSON on the runtime blocking pool so its async worker remains available for image output. Keep complete screen/frame data and FIFO order; report preparation and task failures. The controlled Red becomes Green and seven focused owners pass. Both installed diagnostic hosts pass log audits and cleanup, while drag latency and capture acceptance remain open. S91 tracks an existing stale-response fixture failure reproduced on unchanged source.

- S89-1: Install the diagnostic candidate in fresh host configurations and correlate all 104 completed supplier waits with exact host receipts and answers. Most measured time precedes host receipt; post-answer delay is below 0.521 ms. Both log audits and process cleanup pass, while both drag recordings still exceed the unchanged 33 ms cadence bound. No public release is included.

- S89: Add the exact image name, generation, raster and sequence to the existing consumed-wait event. The served-protocol owner fails on the missing name before correction and passes afterward; all five catalog owners pass. The event remains debug-gated and its interval includes supplier actor and transport work. Installed application delivery remains pending.

- S86-1: Integrate the schema-3 catalog and bounded event writer with
  the current VT 0.0.17 protocol source. The diagnostic service builds
  against the installed native package 0.0.21; 14 writer cases and five
  catalog declaration/emit-name cases pass. Current-core catalog validation
  accepts all 32 events, while its full census reports 60 existing source
  findings tracked by S88. No installed-package or publication pass is
  claimed by this source integration.

- S86: Keep the event-writer batch deadline independent of arriving
  messages. Continuous non-error traffic no longer postpones the 100 ms
  flush until input becomes idle. The new owner reproduces zero stored
  records during 268 ms of continuous input before correction; all 14
  event-contract cases pass afterward, preserving accepted-event order.
  Integration, diagnostic package delivery and publication remain open.

- F239-77-vt-sidecar: Convert the VT 0.0.17 package catalog to schema 3 JSON
  Schema while preserving its event and chain declarations. Red: the current
  core reader reported schema-2 and compact-field findings. Green: the focused
  catalog test and current-core catalog check pass. Runtime acceptance requires
  a fresh install with current schema-3 plugin catalogs; the candidate keeps the
  P47 executable and replay bytes unchanged.

- S85: Prepare the local VT 0.0.17 diagnostics candidate for S84's current
  ordered input transaction. The archive smoke sends a strict insert action and
  required final preedit, checks the correlated acknowledgement and matching
  `render.frame`, then verifies controlled PTY output and exact command bytes
  from ordered engine-trace `feed` chunks. Red: VT 0.0.16 replied
  `invalidParams: input requires bytes or keys field`. The first candidate smoke
  attempt exposed that PTY trace input can be split across read records; the
  check now concatenates decoded chunks in order and compares the exact input.
  The final `make candidate-release` exits 0 and both version/package checks pass
  (2/2). The local darwin-arm64 archive SHA-256 is 6f3b7247bd1e14cc3ce4f25717abd0f5c7ad0e69ca79b6867cf90b5e0bfa7f9c. The
  diagnostics feature is forced and macOS minimum is 14.4. No application
  launch, native input, tag, public release, or registry update was performed.

- S84: Apply native input reports as one ordered VT transaction containing
  actions and a required final preedit snapshot. The service validates all
  fields before side effects, writes ordered action bytes once, applies the
  final preedit once, and includes already-ready PTY output before the final
  frame without waiting for future echo. Red: unchanged input handling rejected
  `one_ordered_input_transaction_writes_actions_once_and_applies_final_preedit`
  because it required the legacy `bytes` or `keys` fields. The unchanged write
  failure response also omitted whether the submitted batch had partially
  reached the PTY. The unchanged key parser also rejected native non-character
  key actions that omit `text`. Green: non-character keys omit `text`, `Char`
  requires string `text`, and null `Char` text or text on another key is
  rejected before side effects. The new
  ordered-action and invalid-late-action cases pass; the ready-echo cursor case
  verifies one frame at column 16 without an intermediate column-12 frame; and
  the existing immediate-echo case preserves its six-glyph baseline and
  split/combined criteria. A controlled write failure also verifies that the
  reply preserves its reason, states that submitted PTY progress is unknown,
  and does not retry. The six focused cases pass. No application runtime,
  activation, package build, or release was performed.

- S83: Prepare a local VT 0.0.16 diagnostics candidate. Ordinary `make release`
  remains non-diagnostic; `make candidate-release` forces
  `FEATURES=diagnostics` and runs the packaged-binary smoke check. Red: the
  unchanged 0.0.15 archive with `SOKSAK_VT_TRACE_DIR` set sent frame
  `{generation:1,raster:1,sequence:1}` with `render:null`. Green: the 0.0.16
  archive reports render metadata for its drawn frame, renders controlled PTY
  output, and records that output in the engine trace. The version and package
  smoke checks pass 2/2; locked Cargo metadata and Node syntax checks pass. The
  local darwin-arm64 archive SHA-256 is
  `f022206b32e5b44f03a7f236bad5b47f26c6c94fc4144039359f6ed28491bb85`. The
  build uses macOS minimum 14.4 and emits no compiler warnings. No GUI, native
  input, application launch, or public release was performed.

- S82: Prepare the local VT 0.0.15 candidate containing S81. Package
  metadata, both Rust crates, and Cargo.lock declare 0.0.15;
  `tests/versions.test.mjs` passes 1/1 and locked Cargo metadata resolves.
  The official debug `make release` target used the declared native
  process-identity package 0.0.21 and the existing core native pkg-config
  dependency. It created
  `soksak-sidecar-vt-alacritty-0.0.15-darwin-arm64.tar.gz`, SHA-256
  `c3532df4b23a92de04cbf9eabb72e782d743d85861901b0cfe99b7158fb735ef`.
  The build uses the declared macOS minimum 14.4 and emitted no compiler
  warnings. The deployment pin is unchanged; the artifact is local and no
  public release or runtime verification is claimed.

- S81: Wrap valid preedit graphemes across terminal rows on a render copy. A viewport keeps the selected caret visible without changing engine cursor or PTY cells; the screen message retains raw `cursor.row` and `cursor.col` for hashing and replay and adds required `cursor.caret` from the raster layout, null when no accepted frame exists. Existing wide-cell spans are cleared whole when overlaid, and one-column raster metadata reports only the visible in-grid span. Red: the unchanged renderer rejected a width-two preedit at the final column; before the wrap-pending correction, `a_full_single_row_preedit_keeps_its_glyphs_visible_at_the_end_boundary` and `a_clipped_wide_grapheme_keeps_its_glyph_visible_at_a_one_cell_end_boundary` left the end caret on a blank row and cropped every preedit glyph. The initial always-object caret failed the no-frame screen message test, and dirty future frames could not advertise a caret. Green: frame tests 31/31, screen-message tests 10/10, cropped render metadata 1/1, and S62 controlled draw-failure recovery 1/1; `cargo fmt --all --check` passes. The Wails error sample did not retain its triggering preedit, so its exact runtime trigger remains unknown. No application, activation, or package release was run.

- S80: Prepare the local VT 0.0.14 archive containing S79. `vt-alacritty/package.json`, both Cargo crates, and `Cargo.lock` declare 0.0.14; `tests/versions.test.mjs` passes 1/1. The official debug `make release` command creates the darwin-arm64 archive with SHA-256 `c95054417477a6a80e111265b1e84d3f514b1e7128ae46a4499287b22a80feda`. The deployment pin is unchanged. The artifact is local and no public release was published.
- S79.1: Add `submission_observer: None` to four `ServeOptions` initializers in the protocol test support module. S79's new observer field otherwise prevents the VT core library test target from compiling. The preserved unchanged-source compile transcript has SHA-256 `405440fa05301622575dfa9997186107e1da5f9b686aa3b9c055920608da5f5`. The affected library test target compiles with `cargo test --offline -p soksak-sidecar-vt-core --lib --no-run`; `cargo fmt --all --check`, records check (104 files), and `git diff --check` pass. No production behavior changes.
- S79: Apply a committed insert and its resulting preedit snapshot in one existing input request and surface command. Red: the fresh split-request fixture rendered 10 glyphs from six committed glyphs plus two marked glyphs; unchanged combined handling submitted frame 4 with six ink glyphs and empty preedit before the combined bytes write started, then frame 5 with eight committed glyphs after the fake echo. Malformed compose + bytes returned an error and a second acknowledgement while writing a byte; malformed bytes + compose changed preedit while returning an uncorrelated error. Green: the same controlled fixture records frame submissions at the OutputSink queue boundary and verifies the combined frame attempts follow the write start and completion; Frame pixels show six glyphs before the echo is processed and eight after, with no duplicate preedit. Paired invalid-field tests verify one correlated error, zero writes, and unchanged preedit. Request acknowledgements are recorded separately from PTY echo. Protocol version, stored formats, standalone input operations and S78.1 metadata are unchanged. No application or package build ran.

- S78.1: Report effective draw colors and CoreText font run counts in frame-bound diagnostics. Glyph metadata carries the resolved sRGB foreground/background after theme defaults and inverse video, including preedit overlays, while original PTY screen cells remain unchanged; each grapheme also reports its positive CoreText run count. Underline references the glyph foreground; non-difference cursors report their draw color and difference-blended block cursors report null. Red: unchanged metadata omitted effective overlay/inverse colors and CoreText run counts. Green: default frame tests 23/23, diagnostic frame tests 26/26, screen-message tests 3/3, same-frame serve contract 1/1, and the Menlo `A` plus Devanagari combining mark fixture verifies multiple CoreText runs and pixel ink inside the reported grapheme bounds. `cargo fmt --all --check` and records check (104 files) pass. The local VT 0.0.13 diagnostic archive SHA-256 is `b8c47330a37b460322bf83003d7eadd6984092c0411496cb18d4ed769ba44c8b`. No public release or runtime capture was performed.

- S78: Add frame-bound renderer geometry to diagnostic screen messages. An explicitly trace-enabled diagnostic service reports raster format and stride, grid and cell metrics, CoreText and underline geometry, cursor bounds, and glyph ink bounds from the same draw as the frame. The default path sends `render: null` and does not compute per-cell bounds. Red: unchanged screen messages lacked the required `render` member, mismatched render/frame identities were accepted, and the Makefile omitted explicit Cargo feature forwarding. Green: 23 default frame tests, 24 diagnostics frame tests, 3 screen-message tests, the service frame/queue correlation case, asymmetric transform unit, default/diagnostics collection gate cases, and all 3 Makefile target tests pass. `cargo fmt --all --check` and records check (104 files) pass. `make release FEATURES=diagnostics` creates a local VT 0.0.13 diagnostic archive, SHA-256 `541f8c2dc283088c6a8f858882c013c90aaaf9df7a8f8cbca35e345c99520264`. No public release or live capture was performed.

- S77.3: Restore compilation of the VT core library test target by initializing the new detach request ledger in four protocol test fixtures. Red: unchanged-source compilation failed with E0063 at all four initializers. Green: all 19 protocol tests and formatting checks pass.

- S77.2: Document the local native-package install and link order. Dependency-sensitive checks run before pnpm local linking; `require.resolve` and required package files are checked before build and release. Keep generated workspace/lock overrides out of commits and preserve the archive before later pnpm resolution. The documented link, build, and release sequence resolves core package 0.0.21 and produces the same local VT 0.0.13 archive hash. No deployment pin or public release changed.

- S77.1: Return declaration read, JSON parse, and display-name validation failures through the build script's `Result` path with their `sidecar.json` context. Red: the unchanged event census rejected all three `unwrap_or_else` panic sites because they had no reason annotation. Green: the census passes; three Cargo build-script interface cases preserve explicit failures for a missing declaration, invalid JSON, and missing `displayName`; the valid macOS arm64 package build passes with the local core native package. `cargo fmt --all --check` and records checks pass. The first test fixture lacked a Cargo target and stopped before the script; the fixture was corrected and all three cases passed.

- S77: Add a correlated `image.detach` request and `image.detached` result that release the current render owner while preserving the terminal session and PTY. Detach is fenced by image name and generation; request replay is bound to its tuple and remains idempotent across reconnect until a newer owner appears. In-flight transfers remain immutable until their exact response or connection end, and late replies must match the full frame identity. Red: the unchanged server did not answer detach, a wrong-frame reply released a newer pending draw, and replay after reconnect returned `requestMismatch`. Green: seven targeted `serve_contract` cases pass. The local VT 0.0.13 macOS arm64 build and official archive command pass; the candidate archive SHA-256 is `7fb7c038dad01828f06c185d82040c5c14d3a7ff761f2593eaf9ed5d764a8a27`. No public release was published. The event census still identifies three missing reason annotations in `vt-alacritty/build.rs`; S77.1 tracks them.

- S75.1: Build and package the local VT 0.0.13 candidate with pnpm's local
  link to core's process identity package 0.0.21. The sidecar deployment
  Git pin remains unchanged. The official build and release commands pass;
  the archive is local and no public release was published. S73-1 remains
  open for fresh installation from the declared Git pin.

- S75: Preedit requires `markedRange`, a null or UTF-16 range relative to `text`; the renderer underlines only graphemes in a valid nonempty range and keeps `selectedRange` selection behavior independent. The protocol input boundary rejects invalid marked ranges before enqueueing preedit, so rejected input does not change surface state. Red: the unchanged renderer failed `preedit_underlines_only_the_marked_utf16_graphemes`, and the unchanged input path failed `an_invalid_marked_range_is_rejected_before_preedit_changes_surface_state` after emitting repeated render errors. Green: 22 frame tests, 23 serve-event tests, three affected serve-contract tests, formatting, the package/crate version check, and records check pass. VT candidate version is 0.0.13; transport protocol remains 1 and preedit changes no reloadable format. Local candidate build is blocked because pnpm's resolved native process-identity package directory is empty and `make build` fails `process-identity-check` with `MODULE_NOT_FOUND`; no release was published.
- S74: The VT sidecar embeds `sidecar.json.displayName` at build time and registers it without reading a host environment variable. Red: the unchanged service reported the missing environment value and did not publish a ready endpoint. Green: its live LaunchServices identity matches the embedded manifest with the variable absent or conflicting; 10 service-cleanup tests and 6 log-dir tests pass, as do formatting and the macOS arm64 build. No release was published.
- S73: The VT service requires its declared process display identity, registers it with LaunchServices before runtime initialization, and keeps its executable filename and first start record. Red: the unchanged service had no LaunchServices entry and did not report a missing identity. Green: all 8 service cleanup tests, scoped event/records checks, formatting, and the local version 0.0.12 build pass on macOS arm64.

- S72: The pre-push hook runs `make push-check`, which runs `make test` (after `make build`) and the commit message check of the pushed range, as the ci workflow does. Red: `tests/make-targets.test.mjs` failed on the missing recipe and the missing hook. Green: 2 of 2 tests pass, and `make push-check` passes.

- S69: The process writes one record as its first action, before any setup: the text record `<UTC time> info sidecar vt: started` on the standard error, which the host stores as a record of the layer `sidecar`, so the time from the spawn of the process to its first own record is visible. The record goes to the standard error and not to the event file, because the file of the contract starts with `log.open`. A clean run now writes exactly that record on its standard error.

- S68: new commits are authored as `min-0-max <max@blue.tools>`; `.mailmap` maps the former name `min-median-max`, and published commits are not rewritten.

- S66: a persistent service ends with its normal cleanup when its service folder is removed (`folder-removed`), when `endpoint.json` is removed (`endpoint-removed`) or when `endpoint.json` names another service (`endpoint-replaced`); the end is recorded as `sidecar.vt.session_stop` with that reason. The service removes `endpoint.json` only while it names itself, so the endpoint of a service that took the folder is kept. The tests of the service remove their folders through a guard that reports a failed removal and remove the socket folder of a service that was killed.

- S67: the tests of `vt-core/tests/event_contract.rs` remove their folders and the configuration folders beside them when they end, whether they pass or fail, and report a removal that fails as `error:`; before, no test removed its folders, so every run left `vt-event-contract-*` entries in the temporary folder.

- S63.3: version 0.0.11 releases S63, S64 and S65.
- S65: the checks of `vt-alacritty/tests/log_dir_test.rs` end the service they started and remove its folder whether they pass or fail, and fail when the service or the folder remains; before, a failed check left the service running with parent 1 and left its folder.
- S64: The default policy stores only `error` in every class, before the first policy and while no `log-policy.json` exists, so `warn`, `info`, `debug` and `trace` events are refused. The golden cases `vt-core/tests/fixtures/log-contract/policy-cases.json` are copied from core. The writers of the tests store every level, so the events they check below error are stored.
- S63.2: an `open` without `image` for a surface that was opened with an image or holds a configured image is refused with `imageAttached` and changes nothing, so the surface keeps drawing the output of its session; before, such an `open` made the surface headless and the output after Enter appeared only when a later key or a focus change drew a frame.
- S63.1: each `open` and each applied `image.configure` records the drawing mode that it leaves on the surface as `sidecar.vt.surface_mode` `{mode, cause}`, so a surface that feeds its output without drawing it is named in the event log.
- S62: a screen whose draw fails stays unsent, so the `screen` event names the next frame and the next change draws it, instead of naming the last sent frame for a screen that no frame holds.
- S59: the `screen` event carries `frame` with the `generation`, `raster` and `sequence` of the frame that shows the screen, so a screen read names the presented frame; `frame` is null without an image state.
- S60: `make hooks` sets `.githooks` as the hook folder, so a commit whose message breaks the commit form or the term rules, or whose records break the record rules, is refused when it is made.
- S58: the test of a blocked write waits for the first byte of the reply instead of a fixed time.
- S57: every crate builds with Rust edition 2024; the manifests declare `edition = "2024"` and the code keeps its behavior. The engine trace folder of a diagnostic build is read once at startup from `SOKSAK_VT_TRACE_DIR`.
- S56: the close of a session ends its child before it takes the writer, so a write that waits for a child that does not read does not hold the close.
- S55: the writes of a session and the owner close of the input loop run outside the runtime workers, so a write that waits for a child that does not read no longer blocks the other tasks of the runtime.
- S54: the service watches the policy file through kqueue, on the folder and on the file, so a change is learned while the log folder is written; `sidecar.vt.policy.watch_started` and `sidecar.vt.policy.watch_event` are declared at level info.
- S49: an answer of the host releases a transfer image that a released surface left only when its name, generation, raster and sequence match, so the `consumed` of the next raster of the same surface reaches the surface and its configure draws.
- S52: the service records a stall of its runtime of 1 s or more as `sidecar.vt.runtime_stall`, the stacks of its threads as `sidecar.vt.stall_dump` and the end as `sidecar.vt.runtime_resumed`.
- S50: `make test` runs the record check `soksak-records`, and CI checks the form of each pushed commit message with `soksak-commits`.
- S40.15: `@soksak/plugin-api` comes from the core release 0.0.11, so `make test` checks the emit sites of the Rust sources against the catalog with the installed `soksak-events`, and the release workflow builds `sok` from that release.
- S51: the manifest case of `tests/events.test.mjs` requires the replay inputs of `sidecar.json` and their declaration in the catalog.
- S48: the writer stores debug and trace in `vt-<boot>.debug.jsonl`, which it creates with the first line of its channel; one seq counts across both files and each rotates by its generations, and `read_run_lines` reads both files of a run in the order of their seq.
- S47: log.dropped names the channel other, because the sidecar holds one event file; the base catalog of core requires the channel.
- S46: the event catalog names the sidecar `vt` beside its schema, which the composed catalog of core requires.
- S44: the engine keeps the input line that a shell redraws after a narrowing reflow; a test replays the recorded bytes of a failed window check (53 to 26 columns) with the rows kept.
- S43: a release that arrives while the event of the previous release waits for its raster is applied after that event instead of being refused.
- S41.6: `sidecar.json` names the event of each kind of replay input in `replay.inputs`.
- S41.2: `census.json` declares the writer entrances of the Rust sources as emitters, and `make test` checks their emit sites against the catalog with `--catalog vt-alacritty/events.json`; the catalog declares the fields `cwd` and `pid` of `sidecar.vt.session_open`.
- S41.5: the `screen` event carries `segment`, the `boot` of the event file that holds the frame record of the same screen.
- S41.4: the `screen` event carries the screen message of core (`sequence` of its frame record, `rows`, `cols`, `cursor`, and per cell `grapheme`, `width`, resolved `fg` and `bg`, `attributes`) instead of `lines` of cells with `ch` and boolean attributes, and a screen sent without a raster stores its frame record too.
- S40.10: `make test` and the CI run the census of core with `census.json`, a panic of a thread is an error record, every reason comment has the form `default:` or `silent:`, and the catalog declares the chain `input` down to `pty_write`.
- S40.13: the service ships the replay executable `soksak-vt-replay`, declared as `replay` in `sidecar.json`, which rebuilds the screens of a session from its stored inputs and prints the hash of each frame record.
- S40.12: the inputs of a session are replay inputs (`pty_read`, `pty_write`, `resize` with `sequence` and `hex`, without a text copy) and the event `frame` is a frame record with `sequence`, `rows`, `cols` and the screen `hash`.
- S40.11: `screen_hash` computes the screen hash of core and reproduces its golden vectors.
- S40.7: a raster without a whole cell, a shell without integration and an inline image command that cannot be applied are recorded.
- S40.9: tests cause the events `consumed_wait`, `read_wait` and `output_blocked` and check their fields.
- S40.8: the event `frame` is stored for every frame with the surface, the session, `sequence`, `size` and `raster`; the hash of the screen waits for the definition of core.
- S40.6: the stop and the end of the service (`session_stop` with the signal or the shutdown request, `session_end`), a client that disconnects, the kill of a session (`kill` with what the signal reached, ESRCH included), the chunk that leaves the output ring and the attachment that the broadcast removes (`buffer_dropped`) are events, and a client task that panicked is an error record.
- S40.3: the reply lines carry the `cids` of their request beside `surface` and `body`, and each reply is the event `sidecar.vt.reply` with `ok`, `micros` and `error` as `<code>: <text>`; the failure to open a headless session names its surface.
- S40.4: the request event does not clone or serialize the whole request: it borrows the text of the line and records a line over 1,000,000 bytes by its head and size, and the writer stores fields over 1,048,576 bytes by their head with `truncated`.
- S40.5: the hex of `pty_write` and `pty_read` is built with a lookup table (8211 allocations for a chunk of 8192 bytes before, at most 32 now), and `pty_read` is built and queued only when the policy stores the class `io`; `log.open` states the policy.
- S40.2: the failures of the event writer are events: each reason of a refusal is its own `log.dropped`, a failed write loses its lines and is stated as `log.dropped` after the next write that succeeds, a failed rotation step and a late flush answer are error records of the place `event writer`.
- S40.1: the event file is schema 2: `log.open` holds `version` and `policy`, `events::read_lines` rejects a file of another schema with an error that names the file and the number found, and the golden fixtures are those of core.
- S39: `sidecar.json` names the event catalog `events.json` in the member `events`, the catalog moved into the package `vt-alacritty`, and the `files` of the package list it, so the release carries the catalog.
- S38: `vt-alacritty/events.json` declares the 15 events of the service (layer, class, level, required fields with their types, description), and `tests/events.test.mjs` and `vt-core/tests/serve_events.rs` check that every declared event is named by a source, every event a source names is declared, and the events of a real session satisfy the catalog.
- S37: the records of the service are the events `sidecar.vt.record.error` and `sidecar.vt.record.info` with the fields `where` and `text`, and the standard error holds only what the process or the system writes. `service_log` is removed, and a failed client authentication, a failed cleanup, a font family that is not installed and a trace folder that cannot be written are records of the event file.
- S36: the events of the performance trace are the events `sidecar.vt.pty_write`, `pty_read`, `pty_eof`, `pty_read_error`, `session_open`, `session_exit`, `frame`, `consumed_wait`, `read_wait`, `output_blocked`, `request`, `session_start` and `client_connect` of the event file, always stored: the level `debug` for `pty_read`, `frame` and `consumed_wait`, `info` for the others. The field `cids` of a request line (a list of strings; another value is answered with a parse error) is written on the events `request` and `pty_write` at the top level of the line, and `SessionPort::write` and `PtyService::write` take it. The flag file `performance` and the module `performance` are removed.
- S35: the service writes its events as JSON lines of the event contract to `<log-dir>/vt-<boot>.jsonl` (mode 0600 in a folder of mode 0700, first line `log.open`, `seq` from 1, rotation at 100 MB with 5 earlier files, `log.dropped` for a refused event). The directory comes from the new required argument `--log-dir <dir>`; the service refuses to start without it, and an argument without a value, a repeated argument and an unknown argument are errors.
- S33: the standard error of the service holds text records of the form `<time> <level> sidecar <where>: <text>`, and a failed client authentication is an error record.
- S32: every write to and read from the PTY is a trace event with its session, length, text and bytes in hexadecimal, the event `request` carries the whole request body, the events `session_open`, `pty_eof`, `pty_read_error` and `session_exit` record the life of a session, and a record is written with one write so that records of several threads are not mixed.
- S31: the performance trace reads the flag of the service directory before each event, so a connection made while the trace was off writes its request and frame events from the moment the host turns the trace on.
- S30: the transfer image checks mark the IOSurface they watch and judge it by that mark, because IOSurface ids are global and a parallel test can receive the id of a released image. `make repeat PACKAGE=<package> COUNT=<n> [TEST=<name>]` repeats tests and reports each run, and `make test` checks rustfmt.
- S29: the `hello` reply of the service carries `version`, the version of this sidecar, so a host tells a running service of another version from the installed one.
- S28: the documents and comments call plugins, sidecars and releases by those words.
