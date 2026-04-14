# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Balance is a programming language for distributed systems built around capability-based service interaction and CSP-style event traces. The workspace is a Rust project with two crates:

- `crates/balance-lang` — language library (lexer, parser, type checker, evaluator, bytecode VM, runtime)
- `crates/balance-cli` — the `balance` binary (CLI entry point)

Language spec and design docs live in `docs/` and `spec/balance.g4` (ANTLR grammar, reference only — the real parser is hand-written in `crates/balance-lang/src/parser`). `README.md` has the language tour.

## Build & run

```sh
cargo build                                # debug build
cargo build --release                      # release; binary at target/release/balance
cargo run -p balance-cli -- <args>         # run CLI without installing
cargo build --features quic                # enable optional QUIC transport
```

CLI subcommands (see `crates/balance-cli/src/main.rs` for full flags):

```sh
balance run <file.bl>      # execute (flags: --entry, --profile, --vm, --event-log, --node-id, --data-dir, --remote-service, --signing-key)
balance check <file.bl>    # type-check only
balance trace <file.bl>    # run and print interaction/event trace (--json for machine output)
balance deploy <spec>      # serve over TCP; spec is file.bl:ServiceName (flags: --bind, --gossip, --seed, --replica-of, --chain-registry)
balance compile <file.bl>  # dump bytecode IR
balance repl
```

## Tests

Unit tests are inline `#[test]` / `#[tokio::test]` blocks throughout `crates/balance-lang/src/**`. There are no integration test dirs (`tests/` at repo root holds `.bl` example programs, not Rust tests). The example programs under `tests/programs/*.bl` are driven by Rust tests that invoke the evaluator/type-checker on them — grep for the file name to find its test.

```sh
cargo test                                          # all tests
cargo test -p balance-lang                          # one crate
cargo test -p balance-lang <substring>              # filter by test name
cargo test -p balance-lang <module>::tests::        # one test module (e.g. types::check::tests::)
cargo test -- --nocapture                           # show stdout
```

## Architecture

The pipeline in `balance-cli::main` is: `tokenize` (lexer/mod.rs, Logos) → `parser::parse` (recursive descent, produces `ast::Program`) → `macro_expand::expand_program` → `types::check::check_program[_with_root]` → either `eval::Evaluator` (default, async tree-walking) or `vm::{compiler, machine}` (bytecode, opt-in via `--vm`, pure computation only; falls back to interpreter for services). If the program has imports, `module::ModuleLoader` resolves them before type checking and the imported items are prepended to `program.items`.

The runtime layer (`crates/balance-lang/src/runtime/`) implements Balance's semantic model. Roughly top-to-bottom (see `docs/architecture.md` for the canonical diagram):

- **Interaction engine** (`interaction.rs`) + **capabilities** (`capability.rs`) — port calls, settlement, observation
- **Resolver** (`resolver.rs`) + **registries** (`registry.rs`, `networked_registry.rs`, `gossip_registry.rs`, `chain_registry.rs`) — capability lookup by publish-id, profile-driven
- **Service runtime** (`service.rs`, `server.rs`, `coordinator.rs`) — hosts service implementations
- **Substrates** — built-ins in `substrate.rs` (Log/KV/Queue/Clock/Crypto), sockets in `socket_substrate.rs`, user-defined via `balance_substrate.rs`; `guarantee.rs` holds the formal laws
- **Events** — `event.rs`, `persistent_event.rs` (NDJSON `--event-log`), `causal.rs`
- **Transport** — `transport.rs` trait + `tcp_transport.rs`; `quic_transport.rs` behind the `quic` cargo feature

`ast/mod.rs` holds all AST node types. `types/check.rs` is the top-level type-check entry; `types/infer.rs` is the inference algorithm. When adding language features, the typical touch-set is: `lexer/mod.rs` (tokens) → `parser/mod.rs` (grammar) → `ast/mod.rs` (nodes) → `macro_expand.rs` (if desugared) → `types/{infer,check}.rs` → `eval/mod.rs` and/or `vm/{compiler,ir,machine}.rs` → a `.bl` example under `tests/programs/` referenced by a Rust test.

## Conventions

- No LSP, no external language server — tooling is the CLI only.
- Async runtime is `tokio` `current_thread` (single-threaded cooperative); `async-recursion` is used because the evaluator recurses through async calls.
- The bytecode VM is **not** feature-complete — it handles pure computation and falls back to the tree-walker for service/substrate interaction. Don't assume VM parity when adding features; wire new semantics into the evaluator first.
- Example programs in `tests/programs/` are the living spec — when you change language semantics, add or update a `.bl` there and its driving Rust test.
