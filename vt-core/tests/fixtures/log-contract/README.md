# Golden fixtures of the event contract

`catalog.json`, `key.jsonl` and `dropped.jsonl` are copies of the golden fixtures of the event contract (core
`packages/log-contract/fixtures`; the contract is core `docs/spec/logging.md`). `vt-core/tests/event_contract.rs`
checks that the lines of the writer of this repository have the structure of these lines. A change of the contract
replaces the three files with the new fixtures.

`policy-cases.json` is the copy of `packages/log-contract/fixtures/policy/cases.json`, `replay-inputs.json` the copy of
`packages/log-contract/fixtures/replay/inputs.json`, and `screen-hash.json` the copy of
`packages/log-contract/fixtures/replay/screen-hash.json`. Core owns all three; a change of the contract replaces the
copies.
