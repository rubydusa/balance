Balance Runtime --- Reference Architecture
=============================================

0\. Design goals
----------------

-   Preserve **semantic model exactly** (no hidden weakening)

-   Make **local execution a fast path of remote semantics**

-   Keep layers **strictly separated**

-   Allow **pluggable transports/substrates**

-   Make **resolution + capability binding first-class**

* * * * *

1\. High-level architecture
===========================

┌──────────────────────────────┐\
│        Entry / REPL          │\
├──────────────────────────────┤\
│     Evaluator / VM           │\
├──────────────────────────────┤\
│   Interaction Engine         │\
├──────────────────────────────┤\
│   Capability Runtime         │\
├──────────────────────────────┤\
│   Resolution Engine          │\
├──────────────────────────────┤\
│   Service Runtime            │\
├──────────────────────────────┤\
│   Substrate Layer            │\
├──────────────────────────────┤\
│   Transport Layer            │\
└──────────────────────────────┘

* * * * *

2\. Core components
===================

2.1 Evaluator / VM
------------------

Balance provides two execution modes:

### Tree-walking evaluator (default)

-   Async evaluator using `tokio` runtime with `async-recursion`

-   Executes AST nodes directly

-   Full feature support: substrate interaction, closures, pattern matching, mutable bindings, I/O, select

-   Single-threaded cooperative async (`current_thread` flavor)

### Bytecode VM

-   Stack-based virtual machine for pure computation

-   Compiler (`vm/compiler.rs`) translates AST to IR (`vm/ir.rs`)

-   Machine (`vm/machine.rs`) executes bytecode instructions

-   Supports: arithmetic, comparisons, closures, method calls, pattern matching, control flow

-   Does **not** support: substrate interactions, capabilities, async operations

-   Activated via `balance run --vm`

### Both modes:

-   execute pure expressions

-   construct interactions (evaluator only)

-   maintain environment

* * * * *

2.2 Interaction Engine
----------------------

Central orchestrator.

### Responsibilities:

-   dispatch interactions

-   track lifecycle (Pending → Executing → Settled/Observed → Completed)

-   enforce:

    -   settlement

    -   observation

### Core structure:

Interaction {\
  id\
  capability\
  operation\
  args\
  state: Pending | Executing | Settled | Observed | Completed\
}

* * * * *

2.3 Capability Runtime
----------------------

Represents bound capabilities.

CapabilityRef {\
  port_type\
  service_id\
  binding (opaque)\
  signature (HMAC-SHA256)\
  authority (@consume | @borrow | @delegate)\
}

Responsibilities:

-   route calls to correct service

-   enforce capability integrity

-   prevent forgery

-   verify authority qualifiers

* * * * *

2.4 Resolution Engine
---------------------

Implements:

resolve Port["name"] with profile P

### Steps:

1.  Query service registry

2.  Filter candidates

3.  Check guarantees

4.  Rank by profile

5.  Return `CapabilityRef`

* * * * *

2.5 Service Runtime
-------------------

Hosts service instances.

### Responsibilities:

-   receive interactions

-   execute provider logic

-   emit events

-   return provisional results

### Internal structure:

ServiceInstance {\
  service_id\
  port_impl\
  components\
  substrates\
}

* * * * *

2.5.1 Service Registration Lifecycle
-------------------------------------

Services become available through a two-phase process during program evaluation:

1.  **Pass 1 (registration)**: The evaluator walks all top-level items and registers
    each `service` declaration with the service registry. This happens before any
    `entry` is executed, ensuring that all locally declared services are resolvable
    by the time user code runs.

2.  **Entry execution**: When the selected entry point begins, all services are
    already registered and can be resolved via `resolve Port["name"]`.

This ordering guarantees deterministic availability: a service declared anywhere
in the program file is always resolvable from any entry point, regardless of
declaration order.

* * * * *

2.6 Substrate Layer
-------------------

Implements mechanisms. Balance provides **seven built-in substrates** plus **user-defined substrates**:

### Built-in substrates

-   **ReplicatedLog** --- ordered, replicated event log (`append`, `read`)

-   **KeyValueStore** --- key-value storage with consistency events (`put`, `get`, `delete`)

-   **Queue** --- FIFO message queue (`enqueue`, `dequeue`, `peek`)

-   **Clock** --- wall-clock time, monotonic counters, timers (`now`, `monotonic`, `sleep`, `elapsed`, `set_timer`, `check_timers`)

-   **Crypto** --- hash, HMAC, AEAD, key exchange, signatures (`sha256`, `hmac_sha256`, AES-GCM, ChaCha20, HKDF, X25519, Ed25519, `random_bytes`)

-   **Socket** --- TCP/UDP networking (`tcp_bind`, `tcp_connect`, `send`, `recv`, `poll`, `setsockopt`)

-   **RawSocket** --- IP-level raw sockets (`open`, `send_to`, `recv_from`, `poll`)

Socket and RawSocket are **pre-registered** in `known_substrates` --- no `.bl` declaration needed.

### User-defined substrates (`BalanceSubstrate`)

-   Defined in `.bl` source with `state`, `uses`, `fn`, `on` clauses, and op bodies

-   Operations run through the **full async evaluator** via `eval_balance_substrate_op()` (supports `await`, closures, full dispatch)

-   Composition via `uses` clauses with dependency resolution through `dep_names` mapping

-   Each instance registered as a service with id format: `substrate/{publish_id}/{comp.name}`

Each substrate provides:

SubstrateInstance {\
  operations\
  event_stream\
}

* * * * *

2.7 Transport Layer
-------------------

Handles communication.

### Responsibilities:

-   send/receive frames

-   multiplex streams

-   enforce transport guarantees

Examples:

-   local (co-located services bypass transport)

-   TCP

-   QUIC

-   custom protocols

* * * * *

### 2.8 Async runtime model

Balance uses a cooperative async runtime built on `tokio`.

#### Runtime configuration

-   CLI uses `#[tokio::main(flavor = "current_thread")]` for single-threaded async

-   All evaluator methods (`eval_expr`, `eval_stmt`, etc.) are `async` via `async-recursion`

#### Concurrency primitives

-   **`concurrent { }` blocks**: launch multiple interactions in parallel, collect results

-   **`select` expression**: I/O multiplexing with cooperative polling (non-blocking poll + `tokio::task::yield_now()`)

-   **`await`**: suspend until interaction completes

#### I/O polling

-   Socket and RawSocket substrates use the `polling` crate (kqueue on macOS, epoll on Linux)

-   File descriptors registered with the poller on socket creation, deregistered on close

-   `poll()` operations with timeout > 0 are intercepted by the evaluator for cooperative async: replaced with a loop of `poll(0)` + `yield_now()`

#### Async substrate operations

-   Clock `sleep` is intercepted: evaluator performs `tokio::time::sleep`, then sets `skip_next_sleep` flag before dispatching to substrate

-   User-defined substrate ops use remove-dispatch-reinsert pattern: substrate is temporarily removed from registry during `eval_balance_substrate_op()`, allowing re-entrant calls to dependency substrates

* * * * *

3\. Interaction lifecycle (runtime)
===================================

Step-by-step
------------

### 1\. Call site

kv.put("a", 1)

→ Evaluator creates:

Interaction {\
  op: put\
  args: ["a", 1]\
}

* * * * *

### 2\. Dispatch

Interaction Engine:

-   locates `CapabilityRef`

-   forwards to Service Runtime

* * * * *

### 3\. Execution

Service:

-   runs provider logic

-   may call substrates

-   returns provisional result (`Ack`, `Observed`, etc.)

* * * * *

### 4\. Event subscription

Interaction Engine:

-   registers interest in required events

Example:

settle log.quorum_committed by ack.key

* * * * *

### 5\. Completion

When matching event appears:

-   interaction marked `Completed`

-   result released to caller

* * * * *

4\. Event system
================

4.1 Event structure
-------------------

Event {\
  type\
  payload\
  timestamp\
  source\
}

* * * * *

4.2 Event streams
-----------------

Each substrate provides:

EventStream {\
  subscribe(filter)\
  poll()\
}

The async EventBus uses `tokio::sync::broadcast` with a sync fast path for same-task events.

* * * * *

4.3 Matching engine
-------------------

Implements:

settle_on / observe_on

Core function:

match(event, predicate) -> bool

`match_event_predicate()` is the canonical matching form. Correlated and frontier matching delegate to it. Supports comparison operators (`Eq`, `Lt`, `Gt`, `Lte`, `Gte`) via `FieldConstraint` in `EventPredicate`.

* * * * *

5\. Scheduler
=============

Requirements:
-------------

-   handle many concurrent interactions

-   non-blocking

-   event-driven

### Model:

-   cooperative scheduling via `tokio` async runtime

-   futures/promises internally

* * * * *

Execution model
---------------

Each interaction is:

Future<Result>

Completion triggered by:

-   direct return (pure)

-   event match (settle/observe)

* * * * *

6\. Local vs Remote unification
===============================

Key rule:
---------

> Local calls go through the same pipeline as remote ones.

### Optimization:

-   short-circuit transport layer

-   direct local dispatch

But:

-   still produce events

-   still enforce semantics

* * * * *

7\. Resolution + registry
=========================

7.1 Service registry
--------------------

Stores:

ServiceDescriptor {\
  name ("kv/main")\
  port\
  guarantees\
  location\
}

* * * * *

7.2 Registry implementations
----------------------------

-   `ServiceRegistry` --- local static registry

-   `NetworkedRegistry` --- distributed registry (TCP)

-   `GossipRegistry` --- gossip-based discovery

-   `ChainBackedRegistry` --- blockchain-backed (future)

* * * * *

8\. Profiles (runtime impact)
=============================

Profiles influence:

-   resolution ranking

-   retry policy

-   transport selection

-   timeout behavior

* * * * *

Example:
--------

local_fast:\
  prefer local transport\
  low retry\
  weak durability acceptable

* * * * *

9\. Retry system
================

Default: **at-least-once**

Implementation:
---------------

-   Interaction Engine retries on:

    -   timeout

    -   transport failure

Requirement:
------------

-   attach stable `interaction.id`

-   providers deduplicate using `Ack.key`

* * * * *

10\. Failure model
==================

Assume:

-   message loss

-   duplication

-   reordering

-   malicious actors

Mitigations:

-   guarantees (e.g. authenticated)

-   substrate verification

-   cryptographic checks

* * * * *

11\. Capability security
========================

Requirements:
-------------

-   unforgeable references

-   bound to service identity

-   cryptographically signed (HMAC-SHA256)

### Representation:

CapabilityRef {\
  service_id\
  public_key\
  token/signature\
  authority\
}

* * * * *

12\. Deployment model
=====================

Local execution
---------------

balance run app.bl

-   starts runtime

-   loads modules

-   runs entry

* * * * *

Service deployment
------------------

balance deploy kv.service:MainKV

-   registers service

-   starts service runtime instance

* * * * *

13\. Minimal runtime API (for implementors)
===========================================

### Core interfaces

interface Evaluator {\
  eval(program) -> Value\
}

interface Resolver {\
  resolve(port, name, profile) -> CapabilityRef\
}

interface InteractionEngine {\
  submit(interaction) -> Future<Result>\
}

interface ServiceHost {\
  handle(interaction) -> ProvisionalResult\
}

interface EventBus {\
  publish(event)\
  subscribe(filter) -> Stream<Event>\
}

* * * * *

14\. Concurrency model
======================

-   implicit concurrency via interactions

-   explicit concurrency via `concurrent { }` blocks and `select` expressions

-   cooperative async via `tokio` single-threaded runtime

-   no shared-memory threading model required

-   state lives in:

    -   services

    -   substrates

* * * * *

15\. Observability (important for real systems)
===============================================

Runtime should expose:

-   interaction traces (`balance trace`)

-   event logs (`--event-log` flag)

-   resolution decisions

This is essential for debugging distributed behavior.

* * * * *

16\. Implementation status
==========================

Phase 1 (COMPLETE)
------------------

-   tree-walking evaluator (async, full-featured)

-   bytecode VM (pure computation)

-   lexer (Logos 0.14), recursive descent parser with Pratt-style precedence climbing (11 levels)

-   type checker with inference

-   local transport + TCP + QUIC networking

-   service registry (local, networked, gossip, chain-backed)

-   event system with substrate-backed settlement/observation

-   seven substrates (ReplicatedLog, KeyValueStore, Queue, Clock, Crypto, Socket, RawSocket)

-   user-defined substrates with state, composition, fn decls, on-clauses

-   guarantee checking with comparison predicates

-   authority enforcement (@consume, @borrow, @delegate) with HMAC-SHA256 signing

-   closures, pattern matching (struct/list/Result/Option), loops, mutable bindings

-   Bytes type, Float type, Result type with `?` operator

-   bitwise operators (6 operators, 11-level precedence)

-   emit statement, select expression, concurrent blocks

-   dynamic import, macros

-   implicit block-level atomicity with compensation

-   cooperative async I/O polling (kqueue/epoll via `polling` crate)

-   REPL, tracing, CLI with multiple commands

Phase 2 (in progress)
---------------------

-   distributed substrate coordination (multi-node quorum)

-   persistent event log (beyond process lifetime)

-   cross-node event forwarding with causal ordering

Phase 3 (in progress)
---------------------

-   retries + failure handling --- implemented (exponential backoff, circuit breaker, profile-driven)

-   profile optimization --- implemented (transport preferences, timeout/retry config)

-   advanced bytecode compilation --- in progress

* * * * *

Final intuition
===============

This architecture enforces the core Balance idea:

> **All effects are interactions.\
> All interactions are justified by events.\
> All events come from substrates.\
> All authority flows through capabilities.**

And critically:

-   nothing is "just a function call"

-   nothing is "just I/O"

-   everything has a place in the model
