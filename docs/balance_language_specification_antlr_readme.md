# Balance Language

## Overview
Balance is a distributed systems programming language where capability-based service interaction is the primary abstraction. It unifies:

- Expression evaluation (pure computation)
- Capability invocation (effects)
- Service publication (distribution)
- Substrate definitions (mechanisms)
- Formal guarantees (semantics)

---

# Quick Reference

## Hello World

### Minimal
```
"Hello, World!"
```

### CLI / Entry
```
entry(out: cap Stdout) {
  out.write("Hello, World!")
}
```

### Service
```
port Hello {
  world() -> String [query]
}

service HelloWorld provides Hello {
  query world() -> String {
    return "Hello, World!"
  }
}
```

---

## Value Types & Literals

| Type | Literal Examples | Description |
|------|-----------------|-------------|
| String | `"hello"`, `"line\n"` | UTF-8 string |
| Int | `42`, `0`, `-1`, `0xFF` | 64-bit signed integer |
| Float | `3.14`, `0.5` | 64-bit floating point |
| Bool | `true`, `false` | Boolean |
| Bytes | `b"deadbeef"`, `b""` | Raw byte sequence (hex-encoded literal) |
| List | `[1, 2, 3]`, `[]` | Ordered collection |
| Map | `{"key": value}`, `{}` | String-keyed dictionary |
| Struct | `Point { x: 1  y: 2 }` | User-defined record (no commas between fields) |
| Result | `ok(value)`, `err(msg)` | Success/failure value |
| Closure | `\|x: Int\| { x + 1 }` | Anonymous function |
| None | `none` | Absence of value |
| Unit | (implicit) | No meaningful value |

---

## Type Methods

### String (13 methods)
| Method | Signature | Description |
|--------|-----------|-------------|
| `len` | `() -> Int` | Character count |
| `contains` | `(sub: String) -> Bool` | Substring test |
| `split` | `(sep: String) -> List<String>` | Split by separator |
| `trim` | `() -> String` | Strip whitespace |
| `to_upper` | `() -> String` | Uppercase |
| `to_lower` | `() -> String` | Lowercase |
| `starts_with` | `(prefix: String) -> Bool` | Prefix test |
| `ends_with` | `(suffix: String) -> Bool` | Suffix test |
| `replace` | `(from: String, to: String) -> String` | Replace all occurrences |
| `substring` | `(start: Int, end: Int) -> String` | Extract range |
| `chars` | `() -> List<String>` | Character list |
| `index_of` | `(sub: String) -> Int` | First index of substring (-1 if absent) |
| `to_bytes` | `() -> Bytes` | UTF-8 encode |

### Int (3 methods)
| Method | Signature | Description |
|--------|-----------|-------------|
| `to_bytes` | `(width: Int) -> Bytes` | Big-endian encoding (1-8 bytes) |
| `to_float` | `() -> Float` | Convert to float |
| `to_string` | `() -> String` | Decimal string |

### Float (7 methods)
| Method | Signature | Description |
|--------|-----------|-------------|
| `floor` | `() -> Float` | Round down |
| `ceil` | `() -> Float` | Round up |
| `round` | `() -> Float` | Round nearest |
| `abs` | `() -> Float` | Absolute value |
| `sqrt` | `() -> Float` | Square root |
| `to_int` | `() -> Int` | Truncate to integer |
| `to_string` | `() -> String` | Decimal string |

### List (13 methods)
| Method | Signature | Description |
|--------|-----------|-------------|
| `len` | `() -> Int` | Element count |
| `push` | `(elem) -> List` | Append (returns new list) |
| `get` | `(idx: Int) -> Any` | Element at index (or None) |
| `map` | `(fn) -> List` | Transform each element |
| `filter` | `(fn) -> List` | Keep matching elements |
| `fold` | `(init, fn) -> Any` | Reduce to single value |
| `contains` | `(elem) -> Bool` | Membership test |
| `concat` | `(other: List) -> List` | Concatenate lists |
| `join` | `(sep: String) -> String` | Join as string |
| `first` | `() -> Any` | First element (or None) |
| `last` | `() -> Any` | Last element (or None) |
| `reverse` | `() -> List` | Reverse order |
| `to_bytes` | `() -> Bytes` | Convert list of ints (0-255) to bytes |

### Map (8 methods)
| Method | Signature | Description |
|--------|-----------|-------------|
| `keys` | `() -> List<String>` | All keys |
| `values` | `() -> List` | All values |
| `entries` | `() -> List` | List of {key, value} structs |
| `contains_key` | `(key: String) -> Bool` | Key existence test |
| `get` | `(key: String) -> Any` | Value for key (or None) |
| `remove` | `(key: String) -> Map` | Remove key (returns new map) |
| `insert` | `(key: String, value) -> Map` | Add/update entry (returns new map) |
| `len` | `() -> Int` | Entry count |

### Bytes (8 methods)
| Method | Signature | Description |
|--------|-----------|-------------|
| `len` | `() -> Int` | Byte count |
| `at` | `(i: Int) -> Int` | Byte value at index (0-255) |
| `slice` | `(start: Int, end: Int) -> Bytes` | Sub-range |
| `concat` | `(other: Bytes) -> Bytes` | Concatenate |
| `hex` | `() -> String` | Hex string representation |
| `to_list` | `() -> List<Int>` | List of byte values |
| `to_string` | `() -> String` | UTF-8 decode (lossy) |
| `to_int` | `() -> Int` | Big-endian signed integer (max 8 bytes) |

### Result (5 methods)
| Method | Signature | Description |
|--------|-----------|-------------|
| `is_ok` | `() -> Bool` | True if Ok variant |
| `is_err` | `() -> Bool` | True if Err variant |
| `unwrap` | `() -> Any` | Extract Ok value (errors on Err) |
| `unwrap_err` | `() -> Any` | Extract Err value (errors on Ok) |
| `unwrap_or` | `(default) -> Any` | Ok value or default |

---

## Operators (by precedence, low to high)

| Prec | Operator | Description |
|------|----------|-------------|
| 1 | `\|\|` | Logical OR |
| 2 | `&&` | Logical AND |
| 3 | `==` `!=` | Equality |
| 4 | `<` `>` `<=` `>=` | Comparison |
| 5 | `\|` | Bitwise OR |
| 6 | `^` | Bitwise XOR |
| 7 | `&` | Bitwise AND |
| 8 | `<<` `>>` | Bitwise shift |
| 9 | `+` `-` | Addition, subtraction |
| 10 | `*` `/` `%` | Multiplication, division, modulo |
| 11 | `-` `!` `~` | Unary negate, not, bitwise complement |
| Post | `.` `()` `[]` `?` | Field/method, call, index, try |

---

## Statements

### Let binding
```
let x = 42
let name: String = "hello"
let mut counter = 0           // mutable binding
```

### Assignment (mutable only)
```
counter = counter + 1
```

### Return
```
return value
return                        // returns Unit
```

### If / else
```
if condition {
  body
} else if other {
  body
} else {
  body
}
```

### Match
```
match value {
  0 => { "zero" }
  n if n > 0 => { "positive" }
  _ => { "negative" }
}
```

### For loop
```
for item in list {
  item.process()
}
```

### While loop
```
while condition {
  body
}
```

### Break / Continue
```
while true {
  if done { break }
  if skip { continue }
}
```

### Emit
```
emit event_type {
  field1: value1
  field2: value2
}
```

---

## Expressions

### Await
```
let result = await cap.operation(args)
```

### Select (I/O multiplexing)
```
select timeout_ms {
  events = socket.poll(handles, 0) => {
    // handle ready events
  }
  else => {
    // timeout
  }
}
```

### Concurrent
```
let results = concurrent {
  service1.query()
  service2.query()
}
```

### Try operator
```
let value = fallible_operation()?    // propagates Err
```

### Closures
```
let add = |a: Int, b: Int| { a + b }
let inc = |x| { x + 1 }             // type-inferred params
list.map(|x| { x * 2 })
```

### Resolve
```
let kv = resolve KV["kv/main"]
let kv = resolve KV["kv/main"] with profile local_fast
```

### Dynamic import
```
import("path/to/module.bl")
```

### Macro call
```
assert!(condition, "message")
```

---

## Pattern Matching

### Patterns
```
match value {
  // Literal
  42 => { ... }
  "hello" => { ... }
  true => { ... }

  // Variable binding
  x => { /* x bound to value */ }
  _ => { /* wildcard */ }

  // Struct destructuring
  Point { x, y } => { /* fields bound */ }
  Point { x: px, y: py } => { /* renamed bindings */ }

  // List destructuring
  [] => { /* empty */ }
  [first, ..rest] => { /* head + tail */ }
  [a, b, c] => { /* exact match */ }

  // Result destructuring
  Ok(value) => { /* success */ }
  Err(msg) => { /* failure */ }

  // Option destructuring
  Some(value) => { /* present */ }
  None => { /* absent */ }

  // Guard clause
  n if n > 0 => { /* conditional */ }
}
```

---

## Declarations

### Port
```
port KV {
  get(key: String) -> String? [query, visible(committed)]
  put(key: String, value: String) -> String [command]
}
```

### Service
```
service MainKV provides KV {
  publish as "kv/main"
  on replicated(5)

  component log = spawn ReplicatedLog()

  command put(key: String, value: String) -> String
    via log.append(KVEntry { key: key  value: value })
    settle log.quorum_committed by ack.key

  query get(key: String) -> String?
    via log.read(key)
    observe log.entry_available by res.frontier
    return res.value
}
```

### Entry
```
entry(kv: cap KV) {
  await kv.put("key", "value")
}

entry main() {
  "Hello"
}
```

### Type (no commas between fields)
```
type Point {
  x: Int
  y: Int
}
```

### Function
```
fn add(a: Int, b: Int) -> Int {
  a + b
}

pure fn double(x: Int) -> Int {
  x * 2
}
```

### Substrate (user-defined)
```
substrate Counter {
  state {
    count: Int
  }

  uses clock: Clock                  // composition dependency

  op increment(amount: Int) -> Int {
    let old = self.count
    self.count = old + amount
    old
  }

  op get_count() -> Int {
    self.count
  }

  fn helper(x: Int) -> Int {        // substrate-local function
    x * 2
  }

  on timer_fired {                   // on-clause
    self.count = self.count + 1
  }

  emits {
    count_changed { old_value  new_value }
  }
}
```

### Guarantee
```
guarantee commit_requires_accept {
  law: quorum_committed(k) => append_accepted(k)
}
```

### Profile
```
profile local_fast {
  locality: "local"
  durability: "weak"
}
```

### Macro
```
macro assert(cond: Expr, msg: Expr) {
  if !cond {
    err(msg)
  }
}
```

---

## Built-in Substrates

### ReplicatedLog
| Op | Params | Returns | Kind |
|----|--------|---------|------|
| `append` | `(value)` | `String` (ack key) | Command |
| `read` | `(offset: Int)` | Value or None | Query |

Events: `append_accepted`, `quorum_committed`, `entry_available`

### KeyValueStore
| Op | Params | Returns | Kind |
|----|--------|---------|------|
| `put` | `(key: String, value)` | `String` (ack key) | Command |
| `get` | `(key: String)` | Value or None | Query |
| `delete` | `(key: String)` | `String` (ack key) | Command |

Events: `put_accepted`, `put_committed`, `entry_available`, `delete_committed`

### Queue
| Op | Params | Returns | Kind |
|----|--------|---------|------|
| `enqueue` | `(value)` | `String` (ack key) | Command |
| `dequeue` | `()` | Value or None | Query |
| `peek` | `()` | Value or None | Query |

Events: `enqueue_accepted`, `enqueue_committed`, `dequeue_available`

### Clock
| Op | Params | Returns | Kind |
|----|--------|---------|------|
| `now` | `()` | `Int` (epoch ms) | Query |
| `monotonic` | `()` | `Int` (counter) | Query |
| `sleep` | `(ms: Int)` | Unit | Command |
| `elapsed` | `(start_ms: Int)` | `Int` (ms) | Query |
| `set_timer` | `(ms: Int, tag: String)` | Unit | Command |
| `check_timers` | `()` | `List<String>` | Query |

Events: `time_read`, `timer_fired`

### Crypto
| Op | Params | Returns | Kind |
|----|--------|---------|------|
| `sha256` | `(data: String)` | `String` (hex) | Query |
| `hmac_sha256` | `(data: String, key: String)` | `String` (hex) | Query |
| `sha256_bytes` | `(data: Bytes)` | `Bytes` | Query |
| `hmac_sha256_bytes` | `(data: Bytes, key: Bytes)` | `Bytes` | Query |
| `aes_gcm_encrypt` | `(key: Bytes, nonce: Bytes, plaintext: Bytes, aad: Bytes)` | `Bytes` | Query |
| `aes_gcm_decrypt` | `(key: Bytes, nonce: Bytes, ciphertext: Bytes, aad: Bytes)` | `Bytes` | Query |
| `chacha20_encrypt` | `(key: Bytes, nonce: Bytes, plaintext: Bytes, aad: Bytes)` | `Bytes` | Query |
| `chacha20_decrypt` | `(key: Bytes, nonce: Bytes, ciphertext: Bytes, aad: Bytes)` | `Bytes` | Query |
| `hkdf_extract` | `(salt: Bytes, ikm: Bytes)` | `Bytes` | Query |
| `hkdf_expand` | `(prk: Bytes, info: Bytes, length: Int)` | `Bytes` | Query |
| `x25519_keypair` | `()` | `{public: Bytes, secret: Bytes}` | Command |
| `x25519_dh` | `(secret: Bytes, their_public: Bytes)` | `Bytes` | Query |
| `ed25519_keypair` | `()` | `{public: Bytes, secret: Bytes}` | Command |
| `ed25519_sign` | `(secret: Bytes, message: Bytes)` | `Bytes` | Query |
| `ed25519_verify` | `(public: Bytes, message: Bytes, signature: Bytes)` | `Bool` | Query |
| `random_bytes` | `(length: Int)` | `Bytes` | Command |

Events: `crypto_op`

### Socket (TCP/UDP)
| Op | Params | Returns | Kind |
|----|--------|---------|------|
| `tcp_bind` | `(addr: String)` | `String` (handle) | Command |
| `tcp_accept` | `(listener: String)` | `String` (handle) | Command |
| `tcp_connect` | `(addr: String)` | `String` (handle) | Command |
| `send` | `(handle: String, data: Bytes)` | `Int` (bytes sent) | Command |
| `recv` | `(handle: String, max: Int)` | `Bytes` | Command |
| `udp_bind` | `(addr: String)` | `String` (handle) | Command |
| `udp_send_to` | `(handle: String, data: Bytes, addr: String)` | `Int` | Command |
| `udp_recv_from` | `(handle: String, max: Int)` | `Bytes` | Command |
| `poll` | `(handles: List<String>, timeout_ms: Int)` | `List<String>` | Command |
| `close` | `(handle: String)` | `String` | Command |
| `setsockopt` | `(handle: String, option: String, value)` | Unit | Command |

Socket options: `SO_REUSEADDR`, `TCP_NODELAY`, `SO_RCVBUF`, `SO_SNDBUF`, `NONBLOCKING`

Events: `socket_bound`, `socket_connected`, `data_sent`, `data_received`, `socket_closed`

### RawSocket (IP-level, requires elevated privileges)
| Op | Params | Returns | Kind |
|----|--------|---------|------|
| `open` | `(protocol: Int)` | `String` (handle) | Command |
| `bind` | `(handle: String, addr: String)` | `String` | Command |
| `send_to` | `(handle: String, data: Bytes, addr: String)` | `Int` | Command |
| `recv_from` | `(handle: String, max: Int)` | `Bytes` | Command |
| `poll` | `(handles: List<String>, timeout_ms: Int)` | `List<String>` | Command |
| `close` | `(handle: String)` | `String` | Command |
| `setsockopt` | `(handle: String, option: String, value: Bool)` | Unit | Command |

RawSocket options: `IP_HDRINCL`, `SO_REUSEADDR`, `NONBLOCKING`

Events: `raw_opened`, `raw_sent`, `raw_received`, `raw_closed`

---

## CLI Commands

```
balance run <file>           # Execute program
  --profile <name>           # Select profile
  --entry <name>             # Select named entry
  --remote-service <addr>    # Connect to remote service
  --vm                       # Use bytecode VM
  --event-log                # Enable event logging
  --node-id <id>             # Set node identity
  --data-dir <path>          # Persistent data directory

balance check <file>         # Type-check only (no execution)

balance trace <file>         # Run with interaction tracing
  --json                     # JSON output format

balance deploy <file>        # Deploy as service node
  --event-log                # Enable event logging
  --node-id <id>             # Set node identity
  --gossip                   # Enable gossip protocol
  --seed <addr>              # Gossip seed node address

balance compile <file>       # Emit bytecode IR to stdout

balance repl                 # Interactive REPL
                             # Commands: :trace, :events, :quit
```

---

## Complete Example

```
// A key-value service with Result error handling and closures

port Store {
  put(key: String, value: String) -> String [command]
  get(key: String) -> String? [query, visible(committed)]
}

service MainStore provides Store {
  publish as "store/main"

  component kv = spawn KeyValueStore()

  command put(key: String, value: String) -> String {
    let result = await kv.put(key, value)
    result
  }

  query get(key: String) -> String? {
    return await kv.get(key)
  }
}

fn process_items(items: List<String>) -> List<String> {
  items
    .filter(|s| { s.len() > 0 })
    .map(|s| { s.to_upper() })
}

entry main(store: cap Store) {
  // Mutable binding for counting
  let mut count = 0

  let items = ["hello", "world", "", "balance"]
  let processed = process_items(items)

  for item in processed {
    let result = await store.put(count.to_string(), item)
    count = count + 1
  }

  // Result handling with ? operator
  let lookup = await store.get("0")
  match lookup {
    Some(val) => { val }
    None => { "not found" }
  }
}
```

---

End of Specification
