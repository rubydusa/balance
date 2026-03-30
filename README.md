# Balance

Balance is a programming language for distributed systems where **capability-based service interaction** is the primary abstraction. It is a practical embodiment of the **Communicating Sequential Processes (CSP)** paradigm --- independent services communicate exclusively through typed, capability-mediated interactions rather than shared memory, with formal event traces providing the mathematical foundation for reasoning about correctness across process boundaries.

Instead of treating distribution as an afterthought, Balance makes authority, consistency, and distributed semantics explicit and checkable from the ground up.

## Why Balance?

In most languages, a function call and a remote service call look the same but behave very differently. Balance makes this distinction first-class:

- **Capabilities** (`cap T`) are unforgeable references to services --- you can't accidentally call something you don't have authority to use
- **Settlement and observation** make consistency semantics explicit --- a write isn't "done" when it returns, it's done when the event trace confirms it
- **Substrates** define the mechanisms (logs, stores, queues, crypto, networking) with formal guarantees that can be verified
- **Profiles** let the same code run with different trust/latency/durability tradeoffs without changing application logic

## Quick Start

### Install

```sh
cargo build --release
```

The `balance` CLI will be at `target/release/balance`.

### Run a program

```sh
balance run tests/programs/hello_pure.bl
```

### Type-check without running

```sh
balance check tests/programs/hello_service.bl
```

## Examples

### Pure expression

A Balance program can be as simple as a single expression:

```
"Hello, World!"
```

### Functions and closures

```
fn double(x: Int) -> Int {
    x * 2
}

entry() {
    let items = [1, 2, 3, 4, 5]
    let doubled = items.map(|x| { x * 2 })
    let sum = doubled.fold(0, |acc, x| { acc + x })
    sum
}
```

### Error handling with Result

```
fn divide(a: Int, b: Int) -> Result {
    match b {
        0 => return err("division by zero")
        _ => return ok(a / b)
    }
}

entry() {
    let r = divide(10, 2)
    match r {
        Ok(v) => v
        Err(e) => 0
    }
}
```

### Defining and using a service

Services publish capabilities through ports. Callers resolve capabilities at runtime and interact through them:

```
port Hello {
    greet() -> String [query, visible(committed)]
}

service HelloWorld provides Hello {
    publish as "hello/world"
    query greet() -> String {
        return "Hello, World!"
    }
}

entry(out: cap Stdout @delegate) {
    let h = resolve Hello["hello/world"]
    await out.writeln(await h.greet())
}
```

### Key-value store with settlement semantics

Commands require settlement --- the runtime confirms the write actually committed, not just that the call returned:

```
port KV {
    put(key: String, value: String) -> String [command]
    get(key: String) -> String? [query, visible(put_committed)]
}

service MainKV provides KV {
    publish as "kv/main"
    component store = spawn KeyValueStore("kv/store")

    command put(key: String, value: String) -> String
        via store.put(key, value)
        settle store.put_committed by ack.key

    query get(key: String) -> String? {
        return none
    }
}

entry(out: cap Stdout @delegate) {
    let kv = resolve KV["kv/main"]
    await kv.put("name", "Balance")
    await out.writeln("stored")
}
```

### User-defined substrate with state

```
substrate Counter {
    state {
        let mut count: Int = 0
    }

    op increment(amount: Int) -> Int {
        count = count + amount
        emit count_incremented { count: count, amount: amount }
        return count
    }

    op get_count() -> Int {
        return count
    }

    emits { count_incremented }
}
```

## Repository Structure

```
balance/
  Cargo.toml                  # Workspace manifest
  crates/
    balance-lang/              # Core language library
      src/
        lexer.rs               # Tokenizer (Logos 0.14)
        parser.rs              # Recursive descent parser
        ast.rs                 # Abstract syntax tree
        eval/                  # Async tree-walking evaluator
        infer.rs               # Type checker
        vm/                    # Bytecode compiler and VM
          compiler.rs          # AST to bytecode
          ir.rs                # Bytecode IR definitions
          machine.rs           # Stack-based VM
        runtime/
          substrate.rs         # Built-in substrates (Log, KV, Queue, Clock, Crypto)
          socket_substrate.rs  # Socket and RawSocket substrates
          interaction.rs       # Interaction engine
          registry.rs          # Service registries
          event_bus.rs         # Async event system
    balance-cli/               # CLI binary
      src/main.rs              # balance run/check/trace/deploy/compile/repl
  spec/
    balance.g4                 # ANTLR grammar reference (documentation only)
  docs/
    philosophy.md              # Language overview and design rationale
    type_system.md             # Type system (values, capabilities, authority)
    execution_model.md         # Evaluation, interactions, settlement, observation
    architecture.md            # Runtime architecture and implementation status
    module_system.md           # Modules, packages, imports
    balance_language_specification_antlr_readme.md  # Complete quick reference
  tests/
    programs/                  # 48 example .bl programs
      hello_pure.bl            # Minimal expression program
      hello_service.bl         # Service with capability resolution
      kv_store.bl              # Key-value store with settlement
      closures.bl              # Closures and higher-order functions
      result_match.bl          # Result type and pattern matching
      user_substrate.bl        # User-defined substrate with state
      echo_socket.bl           # TCP socket echo server
      composed_substrate.bl    # Substrate composition with `uses`
      ...                      # See tests/programs/ for all examples
      helpers/
        math.bl                # Importable math utilities
        greeter.bl             # Importable greeter module
```

## CLI Commands

```
balance run <file>             # Execute a program
balance check <file>           # Type-check only
balance trace <file>           # Run with interaction tracing
balance deploy <file>          # Deploy as a service node
balance compile <file>         # Emit bytecode IR
balance repl                   # Interactive REPL
```

Run `balance run --help` for all flags (`--profile`, `--entry`, `--vm`, `--event-log`, `--node-id`, etc.).

## Documentation

| Document | Contents |
|----------|----------|
| [Quick Reference](docs/balance_language_specification_antlr_readme.md) | All types, methods, operators, syntax, substrates, and CLI |
| [Philosophy](docs/philosophy.md) | Design rationale and language overview |
| [Type System](docs/type_system.md) | Values, capabilities, authority qualifiers, Result, Bytes, Float |
| [Execution Model](docs/execution_model.md) | Evaluation, interactions, settlement, loops, closures, select |
| [Architecture](docs/architecture.md) | Runtime layers, async model, implementation status |
| [Module System](docs/module_system.md) | Modules, imports, packages, dynamic loading |
| [Grammar](spec/balance.g4) | Formal ANTLR grammar reference |

## Key Concepts

**Capabilities** --- Authority-bearing references to services. Cannot be forged or constructed; only obtained via `resolve` or entry injection. Optionally qualified with `@consume`, `@borrow`, or `@delegate`.

**Ports** --- Typed service interfaces. Operations are annotated as `[command]` (requires settlement) or `[query]` (requires observation).

**Services** --- Implementations of ports. Published with a runtime identity (`publish as "name"`), resolved by callers at runtime.

**Substrates** --- Seven built-in mechanism types (ReplicatedLog, KeyValueStore, Queue, Clock, Crypto, Socket, RawSocket) plus user-defined substrates with state, composition, and event handlers.

**Guarantees** --- Formal laws over event traces that constrain substrate behavior and justify interaction semantics.

**Settlement** --- A command is not complete when it returns; it is complete when a matching event confirms it (e.g., `settle log.quorum_committed by ack.key`).
