# Tokio Cancellation & Structured Concurrency — Audit & Improvement Plan

## Table of Contents

1. [Background & Best Practices](#1-background--best-practices)
2. [Current State Audit](#2-current-state-audit)
3. [Identified Issues](#3-identified-issues)
4. [Improvement Plan](#4-improvement-plan)
5. [References](#5-references)

---

## 1. Background & Best Practices

### 1.1 Structured Concurrency

Structured concurrency guarantees that a program's live call-graph forms a **tree** — no cycles, no dangling nodes. In async Rust, this provides three properties:

- **Cancellation propagation**: Dropping a parent future cancels all child futures.
- **Error propagation**: Errors bubble up to callers who can handle them.
- **Ordering of operations**: When a function returns, all its work (including spawned sub-work) is done.

Violating these leads to data loss, resource leaks, or logic bugs. The fundamental issue: `tokio::spawn` creates **detached** tasks — dropping a `JoinHandle` does *not* cancel the spawned task, it merely detaches it. This is the primary source of unstructured concurrency in tokio programs.

### 1.2 Cancellation Mechanisms (ranked by preference)

| Mechanism | Scope | Cleanup? | Hierarchy? | Best For |
|-----------|-------|----------|------------|----------|
| **`CancellationToken`** (tokio-util) | Multi-task | ✅ Yes | ✅ `child_token()` | Graceful shutdown of task trees |
| **`tokio::select!`** | Intra-task | ✅ Yes | ❌ | Racing futures within one task |
| **broadcast/watch channel** | Multi-task | ✅ Yes | ❌ | Simple signal fan-out |
| **`AtomicBool` flag** | Multi-task | ⚠️ Manual poll | ❌ | Simple cooperative cancel |
| **`JoinHandle::abort()`** | Single task | ❌ No | ❌ | Last resort / hard kill |

### 1.3 Key Principles

1. **Every spawned task should be owned** — store the `JoinHandle` and `.await` it (or use `TaskTracker`).
2. **Use `CancellationToken` for hierarchical shutdown** — parent tokens cascade to children via `child_token()`.
3. **Use `select!` for cancellation-aware waits** — combine the work future with `token.cancelled()`.
4. **Prefer intra-task concurrency** (`join!`, `select!`, `race`) over `spawn` when arity is static and parallelism isn't needed.
5. **Use `TaskTracker`** (tokio-util) to track dynamic sets of spawned tasks and wait for them to finish.
6. **Use `DropGuard`** to auto-cancel a token when scope exits, ensuring cleanup on early returns / panics.
7. **Never use `Ordering::Relaxed` for visibility-critical flags** — use `Ordering::SeqCst` or at least `Acquire`/`Release` pairs.
8. **Wrap long-running operations with `token.run_until_cancelled()`** for cancellation-safe wrappers.

### 1.4 Graceful Shutdown Pattern (tokio official)

```
┌─────────────────────────────────┐
│  1. Detect shutdown trigger     │  ctrl_c / user action / internal error
│  2. Signal all tasks            │  CancellationToken::cancel()
│  3. Wait for tasks to complete  │  TaskTracker::wait().await
│  4. Run final cleanup           │  flush caches, close connections
└─────────────────────────────────┘
```

```rust
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

let token = CancellationToken::new();
let tracker = TaskTracker::new();

// Spawn tasks via tracker
tracker.spawn(my_task(token.child_token()));

// On shutdown:
token.cancel();
tracker.close();
tracker.wait().await;
```

---

## 2. Current State Audit

### 2.1 Architecture Overview

DockOck is an **eframe/egui desktop GUI** application. The async runtime is used for:
- **File parsing** (via `spawn_blocking`)
- **LLM orchestration** (streaming HTTP to Ollama/cloud endpoints)
- **Cache I/O** (via `spawn_blocking`)
- **OpenSpec service** interaction (HTTP)

The GUI thread spawns a `std::thread` which runs `tokio::runtime::Handle::block_on()` to execute async processing.

### 2.2 Task Spawning — All Properly Awaited ✅

| Location | Purpose | Awaited? |
|----------|---------|----------|
| `app.rs:~2009` | Warmup model loading | ✅ `warmup_handle.await` |
| `app.rs:~2216,2282,2374` | File/group LLM processing | ✅ `for handle in llm_handles { handle.await }` |
| `app.rs:~2023` | File parsing (`spawn_blocking`) | ✅ `for handle in parse_handles { handle.await }` |
| `llm/mod.rs:~559-598` | Endpoint warm-up (4 tasks) | ✅ Sequential await loop |
| `cache.rs:~109,132` | Disk cache I/O (`spawn_blocking`) | ✅ Inline `.await` |
| `llm/mod.rs:~1949` | Blocking TCP check | ✅ Inline `.await` |

**No orphaned/detached tasks** — every `JoinHandle` is stored and awaited.

### 2.3 Cancellation — AtomicBool Pattern ⚠️

Current approach in `app.rs`:
```rust
cancel_flag: Arc<AtomicBool>   // set by UI "cancel" button

// Checked at:
// - Before spawning each LLM task
// - After acquiring semaphore permit inside each LLM task
if cancel_flag.load(Ordering::Relaxed) { return; }
```

**What works:**
- Prevents new tasks from starting work.
- In-flight LLM tasks complete naturally (acceptable — aborting mid-stream wastes the partial result).

**What's missing:**
- No cancellation of *waiting* tasks (tasks blocked on semaphore acquire keep waiting).
- No cancellation of long HTTP requests in-flight.
- No hierarchical cancellation (all checks read one global flag).
- `Ordering::Relaxed` provides no happens-before guarantee (theoretically a task could read stale `false` after cancel is set).

### 2.4 Timeout Patterns — Well-Implemented ✅

- **Overall stream timeout**: `tokio::time::timeout(timeout, agent.stream_prompt(...))` in `llm/mod.rs`
- **Per-chunk stall detection**: 60s timeout per `stream.next()` call
- **Prefix cache**: Timeout on generate request
- **OpenSpec**: 300s timeout on HTTP request
- **HTTP client**: 30s connect + 90s read timeouts on reqwest

### 2.5 Channel Usage — Correct but Sync-Only

All channels use `std::sync::mpsc` (synchronous), which is appropriate because:
- The GUI thread uses `recv_timeout` for non-blocking polling.
- The async runtime sends via `Sender` (sync send from async is fine; it never blocks the executor for meaningful time).

### 2.6 Concurrency Limiting — Semaphore ✅

`Arc<Semaphore>` with configurable concurrency (default 3). Permits held during task execution, auto-released on drop.

### 2.7 Error Handling — Comprehensive ✅

- Parse errors caught and reported to UI.
- Task panics caught (`JoinError`) and reported.
- LLM errors caught with retry logic (exponential backoff for 429/503/timeout).
- Failed items tracked and reported.

### 2.8 Resource Cleanup — RAII ✅

- `drop(status_tx)` → signals forwarding thread to exit → `fwd.join()` waits.
- HTTP connections managed by reqwest.
- File handles auto-closed.
- No custom `Drop` implementations needed.

---

## 3. Identified Issues

### Issue 1: Cancellation Doesn't Interrupt In-Flight Work 🔴 Medium Priority

**Problem**: When a user clicks "Cancel":
- Tasks waiting to acquire the semaphore will eventually acquire it and then check the flag — wasting time.
- Tasks performing long HTTP streams will run to completion.
- There's no signal that races against the work future in `select!`.

**Impact**: Cancel button feels unresponsive — the operation keeps running for many seconds after clicking cancel.

### Issue 2: No Structured Cancellation Hierarchy 🟡 Low-Medium Priority

**Problem**: A single `AtomicBool` provides no hierarchy. If we wanted to cancel a specific file's processing without cancelling everything, there's no mechanism for it.

**Impact**: Limited — current UI only has "cancel all". But future features (cancel individual file, retry single file) would need hierarchy.

### Issue 3: `Ordering::Relaxed` on Cancel Flag 🟡 Low Priority

**Problem**: `Ordering::Relaxed` on the AtomicBool doesn't guarantee cross-thread visibility ordering. In practice on x86, this almost never matters (x86 has a strong memory model). On ARM or in theory, a spawned task could briefly see a stale `false`.

**Impact**: Negligible on x86. Theoretically incorrect on weakly-ordered architectures.

### Issue 4: No Overall Deadline for `process_files()` 🟡 Low Priority

**Problem**: While individual operations have timeouts, the entire pipeline (parse → warm-up → LLM → OpenSpec) has no overall deadline.

**Impact**: If multiple phases are slow but each stays under its own timeout, the operation could run much longer than expected. Acceptable for user-initiated desktop processing.

### Issue 5: Repeated Operation Can Accumulate Threads 🟡 Low Priority

**Problem**: Each "Generate" click spawns `std::thread::spawn(move || { handle.block_on(...) })`. If the user rapidly clicks while an operation is running, threads accumulate.

**Impact**: Low — the UI disables the button during processing. But not impossible if there's a race.

### Issue 6: `rag.rs::retrieve()` Is Unnecessarily `async` 🟢 Trivial

**Problem**: `retrieve()` in `rag.rs` is marked `async` but performs no I/O. It's pure CPU work.

**Impact**: None — it just adds an unnecessary state machine wrapper.

---

## 4. Improvement Plan

### Phase 1: Adopt `CancellationToken` (Replaces `AtomicBool`) 🔴

**Goal**: Replace `AtomicBool` with `CancellationToken` for proper cancellation-aware async code.

**Dependencies**: Add `tokio-util` to `Cargo.toml` (likely already a transitive dep).

```toml
[dependencies]
tokio-util = { version = "0.7", features = ["rt"] }  # for CancellationToken + TaskTracker
```

**Changes**:

1. **`app.rs` — Replace `cancel_flag: Arc<AtomicBool>` with `cancel_token: CancellationToken`**:
   ```rust
   // Before
   cancel_flag: Arc<AtomicBool>,
   
   // After
   cancel_token: CancellationToken,
   ```

2. **`app.rs` — Cancel button sets token**:
   ```rust
   // Before
   self.cancel_flag.store(true, Ordering::Relaxed);
   
   // After
   self.cancel_token.cancel();
   // For next operation, create a new token:
   self.cancel_token = CancellationToken::new();
   ```

3. **`app.rs` — Pass child tokens to spawned tasks**:
   ```rust
   // Each spawned task gets its own child token
   let child_token = cancel_token.child_token();
   let handle = tokio::spawn(async move {
       let _permit = sem.acquire().await;
       if child_token.is_cancelled() { return; }
       // ... work ...
   });
   ```

4. **LLM streaming — Use `select!` with cancellation**:
   ```rust
   // In stream_chat_with_progress or callers:
   tokio::select! {
       result = stream.next() => { /* process chunk */ }
       _ = token.cancelled() => { 
           // Clean exit — partial result discarded
           bail!("Cancelled by user");
       }
   }
   ```

5. **Semaphore wait — Race with cancellation**:
   ```rust
   // Before: tasks wait indefinitely for semaphore
   let _permit = sem.acquire().await;
   
   // After: cancel-aware semaphore wait
   let _permit = tokio::select! {
       permit = sem.acquire() => permit.unwrap(),
       _ = token.cancelled() => { return; }
   };
   ```

### Phase 2: Adopt `TaskTracker` for Spawn Management 🟡

**Goal**: Replace `Vec<JoinHandle<_>>` with `TaskTracker` for cleaner task lifecycle.

**Changes**:

1. **`app.rs` — Use TaskTracker for LLM tasks**:
   ```rust
   // Before
   let mut llm_handles = Vec::new();
   llm_handles.push(tokio::spawn(async move { ... }));
   for handle in llm_handles { let _ = handle.await; }
   
   // After
   let tracker = TaskTracker::new();
   tracker.spawn(async move { ... });
   tracker.close();
   tracker.wait().await;
   ```

2. **Benefits**:
   - No need to manually collect handles into a Vec.
   - `tracker.wait()` is cancel-safe.
   - Works naturally with `CancellationToken`.

### Phase 3: Improve Cancellation Responsiveness 🟡

**Goal**: Make cancel actually interrupt in-flight LLM streaming.

**Changes**:

1. **`llm/mod.rs` — Accept `CancellationToken` in `process_file()` and `process_group()`**:
   ```rust
   pub async fn process_file(
       &self,
       // ... existing params ...
       cancel_token: &CancellationToken,  // NEW
   ) -> Result<String> { ... }
   ```

2. **`llm/mod.rs` — Cancel-aware stream loop in `stream_chat_with_progress()`**:
   ```rust
   loop {
       tokio::select! {
           chunk = tokio::time::timeout(wait, stream.next()) => {
               match chunk {
                   Ok(Some(item)) => { /* process */ }
                   Ok(None) => break,
                   Err(_) => bail!("Stream stalled"),
               }
           }
           _ = cancel_token.cancelled() => {
               bail!("Cancelled by user during streaming");
           }
       }
   }
   ```

3. **`llm/mod.rs` — Cancel-aware retry loop**:
   ```rust
   for attempt in 0..=Self::RETRY_DELAYS.len() {
       if cancel_token.is_cancelled() { bail!("Cancelled"); }
       match stream_chat_with_progress(...).await {
           Ok(result) => return Ok(result),
           Err(e) if Self::is_retryable_error(&e) && attempt < max => {
               // Cancel-aware sleep
               tokio::select! {
                   _ = tokio::time::sleep(delay) => continue,
                   _ = cancel_token.cancelled() => bail!("Cancelled during retry backoff"),
               }
           }
           Err(e) => return Err(e),
       }
   }
   ```

### Phase 4: Fix Memory Ordering 🟢

If any `AtomicBool` flags remain (e.g., for non-async code paths), upgrade ordering:

```rust
// Before
cancel_flag.store(true, Ordering::Relaxed);
cancel_flag.load(Ordering::Relaxed);

// After
cancel_flag.store(true, Ordering::Release);
cancel_flag.load(Ordering::Acquire);
```

### Phase 5: Optional — Guard Against Re-Entrant Processing 🟢

```rust
// In start_processing():
if self.processing_active.swap(true, Ordering::AcqRel) {
    return; // Already running
}
```

This prevents thread accumulation from rapid button clicks (even if unlikely in practice).

---

## Implementation Priority

| Phase | Priority | Effort | Impact | Dependency |
|-------|----------|--------|--------|------------|
| Phase 1 | 🔴 High | Medium | High — foundational for all other improvements | None |
| Phase 2 | 🟡 Medium | Low | Medium — cleaner code, safer lifecycle | Phase 1 |
| Phase 3 | 🟡 Medium | Medium | High — user-visible cancel responsiveness | Phase 1 |
| Phase 4 | 🟢 Low | Trivial | Low — theoretical correctness on ARM | None |
| Phase 5 | 🟢 Low | Trivial | Low — edge case prevention | None |

**Recommended order**: Phase 1 → Phase 3 → Phase 2 → Phase 4 → Phase 5

Phase 1 and Phase 3 should be done together since Phase 3 requires passing the token through the call chain.

---

## 5. References

1. **tokio-util CancellationToken**: https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html
2. **tokio-util TaskTracker**: https://docs.rs/tokio-util/latest/tokio_util/task/task_tracker/struct.TaskTracker.html
3. **Tokio Graceful Shutdown**: https://tokio.rs/tokio/topics/shutdown
4. **Rust Tokio Task Cancellation Patterns** (blog): https://cybernetist.com/2024/04/19/rust-tokio-task-cancellation-patterns/
5. **Tree-Structured Concurrency** (Yoshua Wuyts): https://blog.yoshuawuyts.com/tree-structured-concurrency/
6. **Let Futures Be Futures** (without.boats): https://without.boats/blog/let-futures-be-futures/
7. **mini-redis reference implementation**: https://github.com/tokio-rs/mini-redis/blob/master/src/shutdown.rs
8. **Tokio JoinHandle docs**: https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html
9. **futures-concurrency** (structured primitives): https://docs.rs/futures-concurrency/latest/futures_concurrency/

### Key Takeaways from Research

- **Dropping a `JoinHandle` does NOT cancel the task** — it detaches it. Use `.abort()` or `CancellationToken` instead.
- **`CancellationToken::child_token()`** creates hierarchical cancellation — cancelling a parent cancels all children, but not vice versa.
- **`CancellationToken::drop_guard()`** auto-cancels the token when the guard is dropped, ensuring cleanup on early returns and panics.
- **`run_until_cancelled(future)`** is a convenience method that wraps `select!` — returns `Some(result)` on normal completion, `None` on cancellation.
- **`TaskTracker`** replaces the pattern of `Vec<JoinHandle>` + manual await loop, handling edge cases (panics, cancellation) more robustly.
- **Structured concurrency = tree-shaped ownership**: every task has a parent, cancellation flows down, errors flow up, and returning means "done".
