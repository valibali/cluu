---
source: Context7 API + docs.rs
library: postcard
package: postcard
topic: to_allocvec and from_bytes for no_std alloc serialization
fetched: 2026-05-19T00:00:00Z
official_docs: https://docs.rs/postcard/latest/postcard/
version: 1.1.3
project_config: version = "1.0", default-features = false, features = ["alloc"]
---

## `postcard::to_allocvec`

**Crate feature required**: `alloc`

Signature:
```rust
pub fn to_allocvec<T>(value: &T) -> Result<Vec<u8>>
where
    T: Serialize + ?Sized,
```

Serialize a `T` to an `alloc::vec::Vec<u8>`.

### Example

```rust
use postcard::to_allocvec;

let ser: Vec<u8> = to_allocvec(&true).unwrap();
assert_eq!(ser.as_slice(), &[0x01]);

let ser: Vec<u8> = to_allocvec("Hi!").unwrap();
assert_eq!(ser.as_slice(), &[0x03, b'H', b'i', b'!']);
```

### With structs (alloc feature)

```rust
use core::ops::Deref;
use serde::{Serialize, Deserialize};
use postcard::{from_bytes, to_allocvec};
extern crate alloc;
use alloc::vec::Vec;

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq)]
struct RefStruct<'a> {
    bytes: &'a [u8],
    str_s: &'a str,
}
let message = "hElLo";
let bytes = [0x01, 0x10, 0x02, 0x20];
let output: Vec<u8> = to_allocvec(&RefStruct {
    bytes: &bytes,
    str_s: message,
}).unwrap();

assert_eq!(
    &[0x04, 0x01, 0x10, 0x02, 0x20, 0x05, b'h', b'E', b'l', b'L', b'o',],
    output.deref()
);
```

---

## `postcard::from_bytes`

**No feature gate** (always available)

Signature:
```rust
pub fn from_bytes<'a, T>(s: &'a [u8]) -> Result<T>
where
    T: Deserialize<'a>,
```

Deserialize a message of type `T` from a byte slice. The unused portion (if any) of the byte slice is **not returned**. If you need the remaining bytes, use `take_from_bytes` instead.

### Example

```rust
use postcard::from_bytes;

let bytes = [0x01]; // serialized `true`
let val: bool = from_bytes(&bytes).unwrap();
assert!(val);
```

### Round-trip with to_allocvec

```rust
use postcard::{from_bytes, to_allocvec};

let original = "test message";
let ser: Vec<u8> = to_allocvec(&original).unwrap();
let deser: &str = from_bytes(&ser).unwrap();
assert_eq!(deser, original);
```

---

## Project Configuration (cluu workspace)

Workspace root `Cargo.toml`:
```toml
postcard = { version = "1.0", default-features = false, features = ["alloc"] }
```

This enables `no_std` compatibility (default-features off) with `alloc` support, giving access to:
- `to_allocvec` (requires `alloc` feature)
- `from_bytes` (always available, no feature gate)
- `to_allocvec_cobs` (requires `alloc` feature)
- `take_from_bytes` (always available)

### Usage in project crates

All consumer crates (`libcluu`, `cluu_proto`, `procmgr`) use `postcard = { workspace = true }` and consistently call:
- `postcard::to_allocvec(&data)` for serialization
- `postcard::from_bytes(&bytes)` for deserialization

API is correct — matches current postcard 1.1.3 docs.