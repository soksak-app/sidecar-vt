# Changelog

[한국어](CHANGELOG.ko.md)

## Unreleased

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
