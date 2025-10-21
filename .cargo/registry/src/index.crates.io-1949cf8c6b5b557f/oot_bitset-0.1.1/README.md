# OoT Bitset

*A no‑frills, zero‑overhead flag system inspired by **The Legend of Zelda: Ocarina of Time***

**Implemented in C / C++, *and* Rust**

Need to pack **hundreds (or thousands) of one‑bit flags**—“talked to an NPC”, “opened a chest”, etc.—into a save file without wasting bytes?  *Ocarina of Time* solved this by storing flags in an array of `uint16_t` words. **oot\_bitset** offers the same trick in either language, with zero runtime overhead.

---

## Why use it?

* **Space‑efficient** – 1 × `u16` word ≙ 16 flags. Scale from 1 to 4096 words (65 536 flags).
* **Zero‑cost abstractions** – branch‑free bit‑twiddling; compiles to a handful of instructions.
* **Header‑only / single‑crate** – drop a header (C) or add a tiny dependency (Rust). No heap, no `alloc`.
* **Infinitely scalable** – need 10 flags or 10 000? Just resize the array.
* **Proven in‑game design** – directly mirrors *OoT*’s save‑file format.

---

## Installation

### C / C++

1. Copy **`oot_bitset.h`** somewhere in your include path.
2. Compile with any C99 (or later) compiler—no extra flags required.

```bash
cc -std=c99 my_game.c -o my_game
```

### Rust

Add the crate to your *Cargo.toml*:

```toml
[dependencies]
oot_bitset = "0.1"
```

---

## Quick start

### C example

```c
#include "oot_bitset.h"
#include <stdio.h>

enum GameEvents {
    FLAG_MET_RUTO_FIRST_TIME        = 0x00, // word 0, bit 0
    FLAG_TALKED_TO_MALON_FIRST_TIME = 0x02, // word 0, bit 2
    FLAG_SAW_BOB   = 0x10,                  // word 1, bit 0
    FLAG_SAW_ALICE = 0x1A                   // word 1, bit 10
};

int main(void) {
    uint16_t flags[30] = {0};   // 30 words ⇒ 480 flags

    bitset_set(flags, FLAG_SAW_BOB);
    printf("Saw Bob? %s\n", bitset_get(flags, FLAG_SAW_BOB) ? "yes" : "no");
}
```

Compile:

```bash
cc -std=c99 demo.c -o demo
```

### Rust example

```rust
use oot_bitset::{bitset_get, bitset_set};

#[repr(u16)]
enum GameEvents {
    MetRutoFirstTime        = 0x00, // word 0, bit 0
    TalkedToMalonFirstTime  = 0x02, // word 0, bit 2
    SawBob                  = 0x10, // word 1, bit 0
    SawAlice                = 0x1A, // word 1, bit 10
}

fn main() {
    let mut flags = [0u16; 30]; // 30 words ⇒ 480 flags

    bitset_set(&mut flags, GameEvents::SawBob as u16);
    println!("Saw Bob? {}", bitset_get(&flags, GameEvents::SawBob as u16));
}
```

Run:

```bash
cargo run --example basic
```

---

## API reference

### C functions & macros

```c
#define bitset_word(set, flag)  ((set)[bitset_index(flag)])
static inline uint16_t bitset_index(uint16_t flag); // word (0–4095)
static inline uint16_t bitset_mask(uint16_t flag);  // bit  (0–15)
static inline bool     bitset_get (uint16_t *set, uint16_t flag);
static inline void     bitset_set (uint16_t *set, uint16_t flag);
static inline void     bitset_clear(uint16_t *set, uint16_t flag);
```

### Rust equivalents

```rust
pub const fn bitset_index(flag: u16) -> usize;
pub const fn bitset_mask(flag: u16) -> u16;

pub fn bitset_get  (set: &[u16],   flag: u16) -> bool;
pub fn bitset_set  (set: &mut [u16], flag: u16);
pub fn bitset_clear(set: &mut [u16], flag: u16);
pub fn bitset_word_mut(set: &mut [u16], flag: u16) -> &mut u16;
```

All functions are `#[inline(always)]` and **panic** if the slice is too short.

---

## Sizing the array

```
max_flags = words × 16
words     = ceil(max_flags / 16)
```

Use as few or as many words as your project needs. *OoT* used 30 words (480 flags), but nothing stops you from using 1 word (16 flags) or 4 096 words (65 536 flags).

---

## Flag encoding

| Bits | 15…4 *(12 bits)* | 3…0 *(4 bits)* |
| ---- | ---------------- | -------------- |
| Use  | **word index**   | **bit index**  |
| Max  | 0–4095 words     | 0–15 bits      |

Because each hex digit is 4 bits, you can read a flag as “word\:bit”.
Example: `0x1AC` → word 26, bit 12.

---

## Example output

```
Words[0] = 0x0004   // FLAG_TALKED_TO_MALON_FIRST_TIME
Words[1] = 0x0401   // FLAG_SAW_BOB | FLAG_SAW_ALICE
```
