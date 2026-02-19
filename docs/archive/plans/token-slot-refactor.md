# Token Slot Refactoring Plan

## Overview

Refactor the token slot layout to be cleaner, more consistent, and extensible.

### Current Problems
- Magic numbers scattered across services (`SVC_TOKEN_LISTEN = 7` duplicated everywhere)
- No single source of truth for slot layout
- Confusing mix of capabilities and endpoints
- Service-specific slots that are actually redundant

### New Design

```
Slots 0-3:  I/O streams (stdin, stdout, stderr, stdlog)
Slots 4-7:  Core capabilities (SELF, SPACE, IPC, CLOCK)
Slot 8:     Registry endpoint
Slots 9-15: Contextual (drivers get device caps, others empty)
```

---

## Phase 1: Define New Constants

**Goal**: Establish single source of truth in `libcluu/src/boot.rs`

### Tasks

- [ ] **1.1** Update `libcluu/src/boot.rs` with new slot constants
  - Add `TOKEN_SELF = 4`
  - Move `TOKEN_SPACE` to 5
  - Rename `TOKEN_PROC_CAP` → `TOKEN_IPC` at slot 6
  - Move `TOKEN_CLOCK` to 7
  - Move `TOKEN_REGISTRY` to 8
  - Add `TOKEN_EXTRA_0..6` for slots 9-15
  - Add slot range constants
  - Add comprehensive documentation

- [ ] **1.2** Update `libcluu/src/lib.rs` re-exports
  - Export new constants
  - Deprecate old names (if keeping temporarily)

---

## Phase 2: Update Init/Wiring

**Goal**: Init uses new slot layout when launching services

### Tasks

- [ ] **2.1** Update `init/src/wiring.rs`
  - Remove `SVC_TOKEN_LISTEN`, `SVC_TOKEN_CAP`, `SVC_TOKEN_IRQ` constants
  - Import new constants from `libcluu::boot`
  - Update `configure_tokens()` to use new slots
  - Update `launch_service()` to set core caps in slots 4-8
  - For drivers: put IRQ/device caps in `TOKEN_EXTRA_0+`

- [ ] **2.2** Update `init/src/services.rs`
  - Review rights definitions for each service kind
  - Ensure procmgr gets elevated TOKEN_SELF rights
  - Ensure vfs gets elevated TOKEN_SPACE rights

- [ ] **2.3** Create `derive_self_cap()` function
  - Derive TOKEN_SELF with appropriate rights per service
  - Normal: THREAD_CONTROL
  - procmgr: + THREAD_SUSPEND, DESTROY

---

## Phase 3: Update Services to Create Own Endpoints

**Goal**: Services no longer expect pre-created listen endpoints

### Tasks

- [ ] **3.1** Update `registry/src/main.rs`
  - Remove `SVC_TOKEN_LISTEN` constant
  - Create own endpoint using `TOKEN_IPC`
  - Already registers with self, so minimal change

- [ ] **3.2** Update `timeserver/src/main.rs`
  - Remove `SVC_TOKEN_LISTEN` constant
  - Create own endpoint: `endpoint_create(token_ipc())?`
  - Register endpoint with registry

- [ ] **3.3** Update `console/src/context.rs`
  - Remove `SVC_TOKEN_LISTEN` constant
  - Create own endpoint using `TOKEN_IPC`
  - Update initialization logic

- [ ] **3.4** Update `tty/src/context.rs`
  - Remove `SVC_TOKEN_LISTEN` constant
  - Create own endpoint using `TOKEN_IPC`

- [ ] **3.5** Update `vfs/src/main.rs`
  - Remove `SVC_TOKEN_LISTEN` constant
  - Create own endpoint using `TOKEN_IPC`

---

## Phase 4: Update Drivers

**Goal**: Drivers use TOKEN_EXTRA slots for device capabilities

### Tasks

- [ ] **4.1** Update `kbd/src/context.rs`
  - Remove `SVC_TOKEN_LISTEN`, `SVC_TOKEN_IRQ` constants
  - Create own endpoint using `TOKEN_IPC`
  - Get IRQ token from `TOKEN_EXTRA_0` (slot 9)

- [ ] **4.2** Update `virtio-blk/src/main.rs`
  - Remove `SVC_TOKEN_LISTEN`, `SVC_TOKEN_CAP` constants
  - Create own endpoint using `TOKEN_IPC`
  - Get IRQ token from `TOKEN_EXTRA_0` (slot 9)
  - Get PCI cap from `TOKEN_EXTRA_1` (slot 10)

---

## Phase 5: Update Procmgr

**Goal**: Procmgr sets up child processes with new slot layout

### Tasks

- [ ] **5.1** Update `procmgr/src/main.rs` constants
  - Remove old `SVC_TOKEN_*` constants
  - Use new constants from `libcluu::boot`

- [ ] **5.2** Update `ProcessManager` struct
  - Rename fields to match new naming (e.g., `ipc_cap` instead of `_proc_cap`)

- [ ] **5.3** Update `map_process_info_page()`
  - Set slots 0-8 for spawned children
  - Leave slots 9-15 empty (or allow future extension)

- [ ] **5.4** Update spawn handling
  - Derive TOKEN_SELF for children
  - Derive TOKEN_SPACE for children
  - Derive TOKEN_IPC for children

---

## Phase 6: Update Libcluu Usage

**Goal**: All libcluu code uses new constants

### Tasks

- [ ] **6.1** Update `libcluu/src/syscall.rs`
  - Replace `TOKEN_PROC_CAP` → `TOKEN_IPC`
  - Add helper functions for new slots

- [ ] **6.2** Update `libcluu/src/runtime.rs`
  - Use new constant names in logging/debug

- [ ] **6.3** Update `libcluu/src/registry.rs`
  - Use `TOKEN_IPC` instead of `TOKEN_PROC_CAP`
  - Use `TOKEN_REGISTRY` with new slot number

- [ ] **6.4** Update `libcluu/src/fd_table.rs`
  - Verify stdio slot usage (should be unchanged)

- [ ] **6.5** Update `libcluu/src/posix/*.rs`
  - Use new constants throughout

- [ ] **6.6** Add convenience functions to `libcluu/src/boot.rs`
  ```rust
  pub fn token_self() -> usize { process_info().tokens[TOKEN_SELF] }
  pub fn token_space() -> usize { process_info().tokens[TOKEN_SPACE] }
  pub fn token_ipc() -> usize { process_info().tokens[TOKEN_IPC] }
  pub fn token_clock() -> usize { process_info().tokens[TOKEN_CLOCK] }
  pub fn token_registry() -> usize { process_info().tokens[TOKEN_REGISTRY] }
  ```

---

## Phase 7: Update Shell and Other Programs

**Goal**: All userspace programs use new constants

### Tasks

- [ ] **7.1** Update `shell/src/main.rs`
  - Use new constants

- [ ] **7.2** Update `shell/src/commands.rs`
  - Replace `TOKEN_PROC_CAP` → `TOKEN_IPC`
  - Replace `TOKEN_SPACE` usage with new slot

- [ ] **7.3** Update `vfs_demo/src/main.rs`
  - Use new constants

---

## Phase 8: Kernel Updates (if needed)

**Goal**: Ensure kernel TOKEN_SELF capability works

### Tasks

- [ ] **8.1** Review `kernel/src/token/` for TOKEN_SELF support
  - May need new ObjectType for thread/self operations
  - Or reuse existing with correct rights

- [ ] **8.2** Review `kernel/src/syscall/handlers.rs`
  - Ensure thread operations check TOKEN_SELF rights
  - `invoke_thread_create` should work with TOKEN_SELF

- [ ] **8.3** Update `kernel/src/bootstrap.rs`
  - Ensure init gets TOKEN_SELF in correct slot

---

## Phase 9: Testing & Verification

**Goal**: Everything works with new layout

### Tasks

- [ ] **9.1** Build all userspace
  ```bash
  cargo xtask userspace
  ```

- [ ] **9.2** Run full system
  ```bash
  make run-debug
  ```

- [ ] **9.3** Verify services start correctly
  - registry registers
  - timeserver responds
  - console renders
  - kbd handles input
  - shell runs

- [ ] **9.4** Test process spawning
  - Shell can spawn programs
  - Programs get correct tokens

---

## Phase 10: Cleanup

**Goal**: Remove all deprecated code

### Tasks

- [ ] **10.1** Remove any deprecated constant aliases
- [ ] **10.2** Remove unused code paths
- [ ] **10.3** Update documentation/comments
- [ ] **10.4** Run clippy and fix warnings
  ```bash
  cargo clippy --workspace
  ```

---

## Summary of Slot Changes

| Name | Old Slot | New Slot | Action |
|------|----------|----------|--------|
| TOKEN_STDIN | 0 | 0 | unchanged |
| TOKEN_STDOUT | 1 | 1 | unchanged |
| TOKEN_STDERR | 2 | 2 | unchanged |
| TOKEN_STDLOG | 3 | 3 | unchanged |
| TOKEN_SELF | - | 4 | **new** |
| TOKEN_SPACE | 6 | 5 | move |
| TOKEN_IPC (was PROC_CAP) | 5 | 6 | rename + move |
| TOKEN_CLOCK | 10 | 7 | move |
| TOKEN_REGISTRY | 4 | 8 | move |
| TOKEN_EXTRA_0 | - | 9 | **new** (replaces SVC_TOKEN_IRQ) |
| TOKEN_EXTRA_1 | - | 10 | **new** (replaces SVC_TOKEN_CAP) |
| TOKEN_EXTRA_2..6 | - | 11-15 | **new** (reserved) |
| SVC_TOKEN_LISTEN | 7 | - | **deleted** |
| SVC_TOKEN_CAP | 8 | - | **deleted** (→ EXTRA_1) |
| SVC_TOKEN_IRQ | 9 | - | **deleted** (→ EXTRA_0) |

---

## Risk Mitigation

1. **Phased approach**: Each phase is independently testable
2. **Constants first**: Define all constants before changing usage
3. **Services before procmgr**: Simpler to debug
4. **Keep old constants temporarily**: Can add aliases during migration

---

## Estimated Effort

| Phase | Complexity | Files |
|-------|------------|-------|
| 1. Constants | Low | 2 |
| 2. Init/Wiring | Medium | 2 |
| 3. Services | Medium | 5 |
| 4. Drivers | Medium | 2 |
| 5. Procmgr | High | 1 |
| 6. Libcluu | Medium | 5 |
| 7. Shell/Programs | Low | 3 |
| 8. Kernel | Low-Medium | 3 |
| 9. Testing | - | - |
| 10. Cleanup | Low | - |

**Total**: ~25 files to modify
