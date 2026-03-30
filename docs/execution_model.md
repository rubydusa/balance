Balance Execution Model
=======================

1\. Core thesis
---------------

Balance programs evaluate in two domains:

1.  **Value domain** --- pure computation

2.  **Interaction domain** --- capability-mediated effects with semantics

The language must make this boundary explicit, composable, and checkable.

* * * * *

2\. Values vs Capabilities
--------------------------

### Values

-   immutable by default (use `let mut` for mutable bindings)

-   freely copyable

-   no authority

-   no side effects

Examples:

"hello"\
42\
3.14\
b"deadbeef"\
[1, 2, 3]\
{"key": "value"}\
ok(42)\
{ x: 1 }

* * * * *

### Capabilities (`cap T`)

A capability is:

-   a **typed reference to a service**

-   carrying **authority**

-   bound to a **resolution result**

-   possibly remote

cap KV\
cap Stdout

Capabilities are:

-   **not forgeable**

-   **not constructible directly**

-   only obtained via `resolve` or injection (e.g. `entry`)

* * * * *

3\. Evaluation model
--------------------

### 3.1 Pure evaluation

E ⊢ expr ⇓ value

Standard expression evaluation:

-   deterministic

-   no side effects

-   no interaction

* * * * *

### 3.2 Capability invocation

E ⊢ cap.op(args) ⇓ interaction

This does **not** immediately produce a value.

Instead, it produces an **interaction object**:

Interaction {\
  op: Operation\
  target: Capability\
  args: Value[]\
  semantics: PortAnnotation\
}

* * * * *

### 3.3 Loops

Balance supports two loop forms.

#### For-in loop

Iterates over a list:

```
for item in [1, 2, 3] {
  item * 2
}
```

#### While loop

Condition-based loop:

```
let mut i = 0
while i < 10 {
  i = i + 1
}
```

#### Break and continue

Both loop forms support `break` (exit the loop) and `continue` (skip to next iteration):

```
for item in items {
  if item == "skip" { continue }
  if item == "stop" { break }
  process(item)
}
```

* * * * *

### 3.4 Closures

Closures are anonymous functions that capture their enclosing scope.

#### Syntax

```
|x: Int, y: Int| { x + y }    // typed parameters
|x| { x * 2 }                 // inferred types
|| { 42 }                      // no parameters
```

Note: no return type annotation. The syntax is `|params| { body }`.

#### Captures

Closures capture variables from their enclosing scope by value:

```
let factor = 3
let scale = |x: Int| { x * factor }
scale(10)  // 30
```

#### First-class usage

Closures are values and can be stored, passed, and returned:

```
let ops = [|x| { x + 1 }, |x| { x * 2 }]
[1, 2, 3].map(|x| { x * x })
[1, 2, 3].filter(|x| { x > 1 })
[1, 2, 3].fold(0, |acc, x| { acc + x })
```

* * * * *

### 3.5 Pattern matching

Match expressions destructure values and dispatch on their shape.

#### Variable and literal patterns

```
match x {
  42 => { "exact" }
  n => { "bound to n" }
  _ => { "wildcard" }
}
```

#### Struct destructuring

```
match point {
  Point { x, y } => { x + y }
  Point { x: px, y: py } => { px + py }  // renamed bindings
}
```

#### List destructuring

```
match list {
  [] => { "empty" }
  [only] => { "singleton" }
  [first, ..rest] => { first }
}
```

#### Result patterns

```
match result {
  Ok(value) => { value }
  Err(msg) => { msg }
}
```

#### Option patterns

```
match optional {
  Some(value) => { value }
  None => { "absent" }
}
```

#### Guard clauses

```
match n {
  x if x > 0 => { "positive" }
  x if x < 0 => { "negative" }
  _ => { "zero" }
}
```

Match can be used as both a statement and an expression (the last arm's value is the result).

* * * * *

### 3.6 Error handling

#### Result type

Balance uses `Result` for explicit error handling. Values are either `Ok(value)` or `Err(value)`:

```
let good = ok(42)
let bad = err("something failed")
```

#### The `?` operator

The postfix `?` operator provides concise error propagation. Applied to a Result:

-   If `Ok(v)`: unwraps to `v`
-   If `Err(e)`: immediately returns `err(e)` from the enclosing function

```
fn parse_and_double(input: String) -> Result {
  let n = parse_int(input)?    // propagates Err
  ok(n * 2)
}
```

#### Pattern matching on Results

```
match operation() {
  Ok(value) => {
    value.process()
  }
  Err(msg) => {
    err("failed: " + msg)
  }
}
```

#### Result methods

```
result.is_ok()           // Bool
result.is_err()          // Bool
result.unwrap()          // value or runtime error
result.unwrap_or(default) // value or default
```

* * * * *

4\. Interaction lifecycle
-------------------------

Every interaction goes through phases:

### 1\. Construction

Created by calling a capability method.

### 2\. Resolution binding (already done)

The capability already refers to a resolved service.

### 3\. Dispatch

Runtime sends the interaction to the service provider.

### 4\. Execution

Provider executes implementation.

### 5\. Semantic completion

Depends on operation type:

* * * * *

5\. Command semantics (settlement)
----------------------------------

For:

put(Key, Value) -> Ack [command]

Execution returns a provisional result:

ack : Ack

But the interaction is not complete until:

∃ e ∈ EventTrace such that e matches settle condition

Example:

settle log.quorum_committed by ack.key

### Formal:

Interaction completes when:

∃ e ∈ Trace :\
  e.type == quorum_committed\
  ∧ e.key == ack.key

* * * * *

### Key insight

Return value ≠ completion

Completion = **return value + matching event**

* * * * *

6\. Query semantics (observation)
---------------------------------

For:

get(Key) -> Value? [query, visible(committed)]

Execution returns:

Observed<Value?, Frontier>

Where:

-   `value` = result

-   `frontier` = position in event space

Then:

observe log.entry_available by res.frontier

### Formal:

Returned value is valid if:

∃ e ∈ Trace :\
  e.type == entry_available\
  ∧ e.offset >= res.frontier

* * * * *

### Key insight

Query results are not "true" in isolation.

They are **true relative to a visibility boundary**.

* * * * *

### 6.5 Emit statement

The `emit` statement publishes events to the EventBus:

```
emit order_placed {
  order_id: id
  customer: name
  total: amount
}
```

#### Semantics

-   Creates an event with the specified type and fields

-   Publishes to the async EventBus (`tokio::sync::broadcast` channel)

-   Events are available to `on` clauses in services and substrate on-clauses

-   Event flow: emit → EventBus → on-clause handlers / interaction engine

#### Use cases

-   Service-to-service event notification

-   Triggering on-clause handlers

-   Integration with the settlement/observation event system

* * * * *

7\. Event traces
----------------

Each substrate defines an **event trace**:

Trace = ordered set of events

Example event:

append_accepted { key, entry }\
quorum_committed { key, entry }\
entry_available { offset }

Traces may be:

-   partially ordered

-   per-stream ordered

-   globally ordered (rare)

* * * * *

### 7.5 Select expression

The `select` expression provides I/O multiplexing with cooperative polling:

```
select timeout_ms {
  events = socket.poll(handles, 0) => {
    for handle in events {
      let data = await socket.recv(handle, 1024)
      process(data)
    }
  }
  else => {
    // timeout branch
  }
}
```

#### Semantics

-   Each arm has a binding (`events`), an expression (typically `poll()`), and a body

-   The expression is evaluated repeatedly until it produces a "ready" value

-   Readiness: a value is ready if it is truthy AND non-empty (empty list is NOT ready)

-   The optional timeout (first expression after `select`) limits total wait time in milliseconds

-   The `else` branch executes if all arms time out

-   Uses cooperative async polling: non-blocking poll with `tokio::task::yield_now()` between attempts

#### Use cases

-   Multiplexing reads across multiple sockets

-   Implementing event loops with timeout

-   Non-blocking I/O patterns

* * * * *

### 7.6 Concurrent await

The `concurrent` expression executes multiple interactions in parallel:

```
let results = concurrent {
  service1.get("a")
  service2.get("b")
  service3.get("c")
}
```

#### Semantics

-   All expressions inside the `concurrent` block are launched simultaneously

-   Results are collected into a list, preserving declaration order

-   Each expression runs as an independent async task

-   Useful for fan-out queries or independent writes

* * * * *

8\. Guarantees
--------------

Guarantees are **laws over traces**.

Example:

quorum_commit_has_prior_accept:\
  committed(k) ⇒ ∃ accepted(k)

Formally:

∀ e ∈ Trace:\
  if e.type == quorum_committed\
  then ∃ e' ∈ Trace:\
    e'.type == append_accepted\
    ∧ e'.key == e.key

* * * * *

### 8.5 Implicit block-level atomicity

Balance provides implicit atomicity at the block level for substrate operations.

#### Scopes

Three execution contexts are atomic:

1.  **Entry bodies** --- the entire entry block

2.  **Command block bodies** --- each command implementation

3.  **Concurrent blocks** --- each expression within a concurrent block

#### Mechanism

-   The evaluator maintains an `atomic_tx_log` that records all substrate write operations

-   If an error occurs within an atomic scope, all recorded operations are automatically reverted via `compensate_op()` --- each substrate type defines its own compensation logic (e.g., ReplicatedLog removes the appended entry, KeyValueStore restores the previous value)

-   Nested atomic scopes merge into the parent scope on success

#### Guarantee interaction

Comparison predicates in guarantees (e.g., `value<0`, `count>=100`) are checked at both:

-   **Settlement time** (ViaSettle) --- when settling an interaction

-   **Substrate dispatch time** --- when executing substrate operations directly

This ensures that formal guarantees constrain both the interaction model and direct substrate access.

* * * * *

9\. Resolution model
--------------------

resolve Port["name"] with profile P

Produces:

cap Port

* * * * *

### Resolution algorithm (conceptual)

1.  **Find candidates**

    -   services publishing `Port`

    -   matching name or alias

2.  **Filter by profile**

    -   trust domain

    -   locality

    -   cost model

3.  **Verify compatibility**

    -   required guarantees

    -   capability constraints

4.  **Select best match**

    -   policy-driven (latency, trust, etc.)

5.  **Bind capability**

    -   result is opaque reference

* * * * *

### Important property

Capabilities are:

-   **late-bound**

-   **replaceable under profile**

-   **stable during usage (no silent rebinding)**

* * * * *

10\. Profiles
-------------

Profiles parameterize resolution and semantics.

They affect:

-   retry behavior

-   locality preference

-   acceptable guarantees

-   trust assumptions

Example:

local_fast:\
  prefer local\
  allow weaker durability\
  minimize latency

* * * * *

11\. Retry semantics
--------------------

Default: **at-least-once**

So:

cap.put(x)

may execute multiple times.

Therefore:

-   commands should be idempotent or deduplicated

-   `Ack.key` is used for correlation

* * * * *

12\. Failure model
------------------

Default assumption: **byzantine environment**

So:

-   messages may be:

    -   dropped

    -   duplicated

    -   reordered

    -   maliciously altered (unless guarantees prevent it)

Guarantees constrain this.

* * * * *

13\. Entry execution
--------------------

entry(out: cap Stdout) {\
  out.write("Hello")\
}

Execution:

1.  Runtime resolves required capabilities

2.  Injects them

3.  Evaluates block

4.  Applies interaction semantics

* * * * *

14\. Local vs remote (unified)
------------------------------

There is **no semantic difference** between:

local service\
remote service

Only differences in:

-   latency

-   guarantees

-   profiles

Local execution is just:

> a degenerate case of remote execution with zero network cost

* * * * *

15\. Desugaring model
---------------------

All sugar must reduce to:

-   `command` (with `settle` clause) — defines settlement semantics

-   `query` (with `observe` clause or `visible` annotation) — defines observation semantics

-   `settle_on { predicate }` — canonical settlement matching form

-   `observe_on { predicate }` — canonical observation matching form

-   explicit selectors

Example:

settle log.quorum_committed by ack.key

↓

settle_on log.quorum_committed {\
  key == ack.key\
}

* * * * *

16\. The core invariant
-----------------------

Every interaction must answer:

1.  **Who authorized this?** → capability

2.  **What does completion mean?** → settlement

3.  **What does this value mean?** → observation

4.  **What guarantees justify it?** → substrate + laws

If those four are clear, the system is well-formed.

* * * * *

Final intuition
===============

Balance is essentially making this explicit:

> A program is not just computation.\
> It is a sequence of **claims about interactions**, justified by **events**, under **guarantees**, using **authority-bearing references**.
