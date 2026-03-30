
1\. Language Overview
---------------------

Balance is a distributed systems language built around:

-   **Values** (pure computation)

-   **Capabilities (`cap T`)** (authority-bearing references)

-   **Ports** (interfaces)

-   **Services** (published implementations)

-   **Resolution** (capability acquisition)

-   **Settlement / Observation semantics**

-   **Substrates + Guarantees** (lower-layer correctness)

* * * * *

2\. Core Constructs
===================

2.1 Values
----------

-   Immutable by default (use `let mut` for mutable bindings)

-   Copyable

-   Serializable

String, Int, Float, Bool, Bytes, List<T>, Map<K,V>, Result, Closure, None, Unit, User types

Balance also supports closures (`|x| { body }`), loops (`for`/`while` with `break`/`continue`), pattern matching (`match` with struct/list/Result/Option destructuring and guards), and error handling (Result type with `?` operator).

* * * * *

2.2 Capabilities
----------------

cap KV\
cap Stdout

Properties:

-   Cannot be constructed directly

-   Only introduced via:

    -   `resolve`

    -   `entry` injection

    -   parameter passing

-   Not serializable by default

-   Authority enforced via `@consume`, `@borrow`, `@delegate` qualifiers (HMAC-SHA256 signed)

* * * * *

2.3 Ports
---------

Define service interfaces.

port KV {\
  get(Key) -> Value? [query, idempotent, visible(committed)]\
  put(Key, Value) -> Ack [command]\
}

Annotations:

-   `command` → requires settlement

-   `query` → requires observation

-   `idempotent` → retry-safe

-   `visible(...)` → visibility requirement

* * * * *

2.4 Services
------------

Provide ports and define runtime behavior.

service MainKV provides KV {\
  publish as "kv/main"\
  on replicated(5)

  command put(key, value) -> Ack\
    via index.put(KVEntry(key, value))\
    settle log.quorum_committed by ack.key

  query get(key) -> Value?\
    via index.get(key)\
    observe log.entry_available by res.frontier\
    return res.value\
}

* * * * *

2.5 Resolve
-----------

let kv = resolve KV["kv/main"] with profile public_strong

Returns:

cap KV

* * * * *

2.6 Entry
---------

Program entrypoint.

entry(out: cap Stdout) {\
  out.write("Hello, World!")\
}

Rules:

-   Parameters must be values or capabilities

-   Injected by runtime

-   Returns last expression

* * * * *

3\. Execution Model
===================

3.1 Pure evaluation
-------------------

expr → value

No side effects.

* * * * *

3.2 Capability invocation
-------------------------

cap.op(args) → Interaction<T>

* * * * *

3.3 Interaction lifecycle
-------------------------

1.  Construct

2.  Dispatch

3.  Execute

4.  Complete via:

    -   **Settlement (commands)**

    -   **Observation (queries)**

* * * * *

3.4 Command completion
----------------------

settle log.quorum_committed by ack.key

Completion condition:

∃ event matching selector

* * * * *

3.5 Query validity
------------------

observe log.entry_available by res.frontier

Returned value is valid relative to event trace.

* * * * *

4\. Type System
===============

4.1 Type categories
-------------------

| Kind | Examples |
| --- | --- |
| Value | `String`, `Int`, `Float`, `Bool`, `Bytes`, `List<T>`, `Map<K,V>`, `Result`, `Closure` |
| Capability | `cap KV`, `cap KV @consume` |
| Structured | `Observed<T,F>` |

* * * * *

4.2 Capability rules
--------------------

-   No construction

-   Explicit passing

-   No implicit serialization

-   Authority qualifiers enforced (`@consume`, `@borrow`, `@delegate`)

* * * * *

4.3 Standard types
------------------

type Ack {\
  key: EventKey\
}

type Observed<T, F> {\
  value: T\
  frontier: F\
}

* * * * *

4.4 Provider obligations
------------------------

-   `command` must include `settle`

-   `query` must include `observe` OR explicit justification

* * * * *

4.5 Optional types
------------------

Value? ≡ Option<Value>

Pattern matching with `Some(v)` / `None`.

* * * * *

5\. Resolution Model
====================

5.1 Input
---------

resolve Port["name"] with profile P

5.2 Algorithm
-------------

1.  Find matching services

2.  Filter by profile

3.  Verify guarantees

4.  Select best candidate

5.  Bind capability

* * * * *

5.3 Properties
--------------

-   Late bound

-   Stable after binding

-   Capability is opaque

* * * * *

6\. Profiles
============

Profiles influence:

-   locality

-   retry behavior

-   trust assumptions

-   acceptable guarantees

Example:

local_fast\
public_strong\
trusted_cluster

* * * * *

7\. Retry & Failure
===================

Defaults:

-   Retry: **at-least-once**

-   Failure: **byzantine**

Implications:

-   Commands must be idempotent or deduplicated

-   `Ack.key` used for correlation

* * * * *

8\. Substrates
==============

Define mechanism contracts. Balance provides **seven built-in substrates** plus **user-defined substrates**.

### Built-in substrates

| Substrate | Key Operations | Purpose |
| --- | --- | --- |
| **ReplicatedLog** | `append`, `read` | Ordered, replicated event log |
| **KeyValueStore** | `put`, `get`, `delete` | Key-value storage with consistency events |
| **Queue** | `enqueue`, `dequeue`, `peek` | FIFO message queue |
| **Clock** | `now`, `monotonic`, `sleep`, `elapsed`, `set_timer`, `check_timers` | Time, timers, scheduling |
| **Crypto** | `sha256`, `hmac_sha256`, AES-GCM, ChaCha20, HKDF, X25519, Ed25519, `random_bytes` | Cryptographic primitives |
| **Socket** | `tcp_bind`, `tcp_connect`, `send`, `recv`, `udp_bind`, `poll`, `setsockopt` | TCP/UDP networking |
| **RawSocket** | `open`, `send_to`, `recv_from`, `poll` | IP-level raw socket access |

Each substrate provides:

-   Typed operations (commands and queries)

-   Event streams (traces used for settlement/observation)

-   Guarantees (formal laws)

Socket and RawSocket are **pre-registered** --- they require no `.bl` declaration to use.

* * * * *

### 8.1 User-defined substrates

Beyond the seven built-ins, Balance supports user-defined substrates with full programmability:

```
substrate Counter {
  state {
    count: Int
  }

  uses clock: Clock              // composition: depend on another substrate

  op increment(amount: Int) -> Int {
    let old = self.count
    self.count = old + amount
    old
  }

  fn helper(x: Int) -> Int {    // substrate-local function
    x * 2
  }

  on timer_fired {               // event handler (on-clause)
    self.count = self.count + 1
  }

  emits {
    count_changed { old_value  new_value }
  }
}
```

#### Features

-   **State blocks**: mutable state local to the substrate instance

-   **Op bodies**: operation implementations with full evaluator access (async, closures, await)

-   **Composition (`uses`)**: substrates can depend on other substrates; dependencies are resolved via `dep_names` mapping

-   **Substrate-local functions**: `fn` declarations scoped to the substrate

-   **On-clauses**: event handlers that fire when matching events arrive; run with full async evaluator

-   **Substrate services**: each user-defined substrate instance is registered as a service, accessible via capabilities

* * * * *

9\. Guarantees
==============

Named laws over event traces.

guarantee commit_requires_accept {\
  law:\
    quorum_committed(k) ⇒ append_accepted(k)\
}

Used to:

-   verify substrate correctness

-   justify provider semantics

Guarantees support comparison predicates (`value<0`, `count>=100`) checked at both settlement and dispatch time.

* * * * *

10\. Modules
============

10.1 File = module
------------------

module kv.storage

* * * * *

10.2 Export
-----------

export port KV\
export service MainKV

Default: private (except `fn`, which is exported by default)

* * * * *

10.3 Import
-----------

import kv.storage\
import kv.storage.{KV}\
import kv.storage as storage

Dynamic import at runtime:

import("path/to/module.bl")

* * * * *

10.4 Resolution vs modules
--------------------------

-   Modules → compile-time

-   Services → runtime

* * * * *

11\. Packages
=============

Defined via:

balance.toml

[package]\
name = "kv"\
version = "0.1.0"

[dependencies]\
net = "1.0.0"

* * * * *

12\. Entry Execution
====================

Runtime:

1.  Resolve required capabilities

2.  Inject into `entry`

3.  Execute

4.  Apply interaction semantics

* * * * *

13\. Desugaring Rules
=====================

All sugar must reduce to:

-   `implement`

-   `settle_on`

-   `observe_on`

-   explicit selectors

Example:

settle log.quorum_committed by ack.key

↓

settle_on log.quorum_committed { key == ack.key }

* * * * *

14\. Core Invariants
====================

Balance enforces:

1.  No capability forgery

2.  Explicit authority

3.  Separation of value vs interaction

4.  Commands require settlement

5.  Queries require visibility justification

6.  Runtime identity ≠ compile-time structure

* * * * *

15\. Minimal Programs
=====================

Pure
----

"Hello, World!"

Entry
-----

entry(out: cap Stdout) {\
  out.write("Hello, World!")\
}

Service
-------

service HelloWorld {\
  query world() -> String {\
    return "Hello, World!"\
  }\
}

* * * * *

16\. Non-goals (v0.1)
=====================

-   No full effect system

-   No higher-kinded types

* * * * *

Final Summary
=============

Balance defines a system where:

-   **values compute**

-   **capabilities act**

-   **services publish**

-   **resolution binds**

-   **events justify**

-   **guarantees prove**

And critically:

> **An interaction is not complete when it returns ---\
> it is complete when it is justified.**
