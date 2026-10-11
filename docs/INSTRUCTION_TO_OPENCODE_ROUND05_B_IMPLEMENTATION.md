# Round 05 B1–B5 Implementation Instruction — Design Frozen

> **Implementation phase is authorized by the human owner.**
> This file is the OpenCode implementation entry point for Round 05 B1–B5.
> The design is frozen. Do not reinterpret or expand it.

## 0. Frozen design authority

Before changing code, read all of the following:

1. `AGENTS.md`
2. `MASTER_TASKS.md`
3. `BUG_TRACKER.md`
4. `docs/design/ROUND05_B_DESIGN_DRAFTS.md`
5. `docs/design/ROUND05_B_DESIGN_V2_INSTRUCTION.md`
6. `docs/design/ROUND05_B_DESIGN_REVIEW.md`
7. `docs/design/ROUND05_B_DESIGN_V2_INSTRUCTION.md` section `0. 严格执行边界`

The frozen Design V2 and the human authorization in this task take precedence over earlier `WAITING_AUTHORIZATION` labels in historical documents. Do not rewrite historical audit records to make statuses appear verified.

If the frozen documents disagree with one another, or if implementation requires a decision not explicitly frozen, **stop before editing code** and create a Design Change Request instead of guessing.

## 1. Strict implementation boundary

This is now an implementation task, but only within the frozen Design V2.

### Allowed

- Modify only files explicitly allowed by the relevant B task below;
- Add the required regression and mutation tests;
- Add narrowly scoped supporting types/helpers required by the frozen design;
- Update implementation documentation with factual changed-file and test results;
- Add a Design Change Request if a semantic conflict is discovered.

### Forbidden

- Changing frozen units, authority, state transitions, persistence semantics, matching priority, or transaction behavior;
- Changing RiskConfig thresholds or their meaning;
- Changing strategy semantics, signal direction, price semantics, or trading behavior outside the frozen design;
- Changing cold wallet, EIP-712, or cryptographic standard behavior;
- Adding a third risk authority;
- Using a local workaround that contradicts the frozen design;
- Silently falling back from invalid/unknown data to zero or raw credentials;
- Marking any Finding `VERIFIED` or `CLOSED`;
- Editing audit history to claim independent verification;
- Committing unless separately instructed.

## 2. Execution protocol

Work sequentially: **B1 → B2 → B3 → B4 → B5**.

Before each B task:

1. Read the corresponding frozen design section;
2. List the exact implementation decisions being applied;
3. Check `git status`;
4. Confirm Allowed Files;
5. Implement only that B task;
6. Add tests before or together with the implementation;
7. Run the task-local tests;
8. Run the relevant full suite;
9. Stop if a Design Change Request is required.

Do not parallelize OpenCode workers in the same worktree.

## 3. Baseline and verification commands

Python environment:

```bash
cd 'D:/WorkBuddyData/AI Weather'
C:/Users/32389/.workbuddy/binaries/python/envs/default/Scripts/python.exe -m pytest -q
```

Rust environment:

```bash
export PROTOC=C:/Users/32389/tools/protoc/bin/protoc.exe
cd 'D:/WorkBuddyData/AI Weather/polymarket-rust/polymarket-rust'
cargo test
cargo clippy --all-targets
```

Acceptance requires:

- Python full suite green;
- Rust full suite green;
- clippy 0 error;
- all new boundary/mutation tests green;
- no unapproved files changed.

If Windows locks Rust source files, do not kill Hermes/WorkBuddy. Use a clean `git archive HEAD` copy for verification and report that fact.

---

# B1 — P1-008 Order-Level Reserve Accounting

## Frozen behavior

- BUY reserve enters using `limit_price × quantity`.
- Filled exposure remains based on actual execution price.
- Reserve release is order-level and uses the order limit price for the filled quantity.
- Unknown limit/order identity is not treated as precisely released: keep reserve conservatively and route to unresolved/reconciliation.
- Terminal reserve accounting must be idempotent.
- SELL does not create BUY-style opening reserve.
- No RiskConfig threshold change.

## Allowed Files

- `polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`
- `polymarket-rust/polymarket-rust/src/grpc/service.rs`
- `polymarket-rust/polymarket-rust/src/fast/order_state.rs` only if required for the frozen order identity/transition
- Corresponding Rust tests

## Implementation requirements

- Keep OrderStore/RiskEngine layering one-directional; RiskEngine must not import or depend on gRPC service.
- Resolve exchange order identity and limit price in the service/adapter layer, then call an explicit order-level RiskEngine API.
- Support multiple orders for the same token with different limits.
- Distinguish fill delta from cumulative fill.
- Make full fill, partial fill + cancel, expiry, replayed fill, and replayed cancel idempotent.
- Unknown identity/limit must not silently use execution notional as if exact.
- Preserve current PnL/exposure semantics unless explicitly frozen otherwise.

## Required Tests

- Same token, two different limits, interleaved partial fills;
- Full price improvement;
- Partial fill then cancel;
- Multiple fills then full fill;
- Unknown limit/order identity;
- Duplicate fill and duplicate cancel;
- Fill/cancel concurrency;
- Mutation: restore filled-notional reserve release and ensure tests fail.

## Stop condition

If exact order-level reserve ownership cannot be implemented without changing frozen Risk Model semantics, stop and submit a Design Change Request. Do not approximate with a token-level `min()`.

---

# B2 — P1-009 Unknown Order Reconciliation

## Frozen behavior

- Unknown result is explicitly represented as `UnknownOutcome` or an equivalent in-doubt collection.
- It is included in reconciliation candidates.
- Unknown/in-doubt keeps reserve conservatively.
- TTL never authorizes reserve release by itself.
- Matching priority is:

```text
exact exchange_order_id
→ exact client_order_id
→ unique fingerprint
→ unresolved
```

- Ambiguous fingerprint never auto-claims.
- Restart/replay is idempotent and requires durable persistence if restart recovery is claimed.
- Unresolved state forces TradingGate away from READY.

## Allowed Files

- `polymarket-rust/polymarket-rust/src/fast/order_state.rs`
- `polymarket-rust/polymarket-rust/src/grpc/service.rs`
- `polymarket-rust/polymarket-rust/src/bin/server.rs`
- `polymarket-rust/polymarket-rust/src/fast/state_sync.rs`
- Persistence files only if explicitly included by the frozen Design V2;
- `proto/polymarket.proto` only if the frozen design explicitly requires an observability field;
- Corresponding Rust tests.

## Implementation requirements

- Persist all fields required for restart recovery: client ID, request fingerprint, token, side, price, size, timestamp, reserve identity/state.
- Define canonical fingerprint serialization and comparison tolerance exactly as frozen.
- Use action identity to prevent duplicate fill, release, and terminal transitions.
- Missing/blank trade_id cannot produce an applied fill.
- Maker/taker attribution must remain consistent with existing trade semantics.

## Required Tests

- In-doubt does not enter READY;
- Unique remote open-order match progresses to Submitted;
- Ambiguous fingerprint remains unresolved;
- TTL expiry keeps reserve;
- Restart/replay is a no-op;
- Duplicate trade does not double-apply;
- Missing trade_id does not create fill attribution;
- Durable state survives restart and continues reconciliation.

## Stop condition

If durable persistence or exact matching is not defined in the frozen design, stop and submit a Design Change Request. Do not invent an in-memory-only implementation while claiming restart recovery.

---

# B3 — P1-RISK-UNIT-001 Python/Rust Risk Authority

## Frozen units

| Field | Unit |
|---|---|
| `quantity` | shares/contracts |
| `yes_price` | USDC/share |
| `no_price` | USDC/share |
| `position` | USDC notional |
| `global_exposure` | USDC notional |
| `reserve` | USDC notional |
| position/global limits | USDC |

Notional:

```text
BUY_YES = quantity × yes_price
BUY_NO  = quantity × (1 - yes_price)
```

Never use `quantity × target_price` for both directions.

## Frozen authority

```text
Python RiskGate  = local projection/advisory
Rust RiskEngine  = sole authoritative blocking risk authority
```

Do not create a third authority.

## Allowed Files

Only the files explicitly selected by the frozen Design V2:

- `strategy_engine/aggregator/risk_gate.py`
- `strategy_engine/aggregator/models.py`
- `strategy_engine/aggregator/aggregator.py`
- `strategy_engine/config.py` or the frozen configuration source;
- Rust `risk_engine.rs` only for the frozen authority/config integration;
- Rust `bin/server.rs` only for frozen configuration loading;
- `proto/polymarket.proto` only if explicitly frozen;
- Corresponding Python/Rust tests.

## Implementation requirements

- Make position and exposure calculations dimensionally explicit;
- Implement the frozen BUY_YES/BUY_NO cost formula;
- Define and implement every `position_notionals` write point: fill, sell, settlement, restart, reconciliation;
- Ensure Python does not block using a stale/incomplete second authority if Rust is sole authority;
- Select one configuration transport and remove divergent defaults only as specified by the frozen design;
- Missing, invalid, or inconsistent authority/configuration must fail closed;
- Ensure Python/Rust produce the same result for the same order and state.

## Required Tests

- BUY_YES notional;
- BUY_NO notional;
- low-price high-share order is not falsely rejected;
- position/global units are USDC;
- price improvement;
- partial fill;
- fees;
- Python/Rust consistency;
- configuration single source;
- missing/inconsistent configuration fails closed;
- `position_notionals` write/restart/reconcile chain.

## Stop condition

If implementing B3 requires choosing between multiple authority/configuration designs not explicitly frozen, stop and submit a Design Change Request. Do not choose based on convenience.

---

# B4 — P1-010 Snapshot/Event Authority

## Frozen behavior

- Snapshot and event authority are explicitly defined by Design V2.
- Normal fills are monotonic unless an explicit reconciliation correction carries authority, generation/version, reason, and idempotency key.
- `max()` alone is not an acceptable solution.
- OrderStore, RiskEngine, and FillLedger must converge to one state.
- Restart replay must be idempotent.

## Allowed Files

- `polymarket-rust/polymarket-rust/src/grpc/service.rs`
- `polymarket-rust/polymarket-rust/src/fast/order_state.rs`
- `polymarket-rust/polymarket-rust/src/fast/state_sync.rs`
- `polymarket-rust/polymarket-rust/src/fast/fill_records.rs` only if explicitly frozen;
- Corresponding Rust tests.

## Implementation requirements

- Implement the frozen sequence/version/generation source;
- Use an atomic merge/update path rather than unrestricted `upsert`;
- Prevent stale snapshots from lowering filled size;
- Keep correction/reconciliation separate from normal event application;
- Preserve trade_id/order_id idempotency;
- Document and test lock ordering across snapshot/event/reconciliation paths;
- Do not hide authority conflicts with an unconditional max.

## Required Tests

- stale snapshot after event;
- event after snapshot;
- multiple events after snapshot;
- duplicate event;
- reconciliation correction;
- restart replay;
- filled quantity cannot regress without authorized correction;
- final OrderStore/RiskEngine/FillLedger consistency.

## Stop condition

If the frozen design does not specify the sequence/version authority, stop and submit a Design Change Request. Do not implement timestamp heuristics.

---

# B5 — R5-005 Zeroization

## Frozen behavior

This task must distinguish code-controlled best-effort cleanup from an absolute memory-erasure guarantee.

Trace:

```text
encrypted keystore
→ decrypt Zeroizing buffer
→ LocalWallet/SecretKey
→ clones/Arc
→ signing
→ cache
→ TTL/lock/sweep/drop
```

## Allowed Files

- `polymarket-rust/polymarket-rust/src/wallet/keystore.rs`
- `polymarket-rust/polymarket-rust/src/wallet/manager.rs`
- `polymarket-rust/polymarket-rust/src/fast/hot_wallet.rs`
- `polymarket-rust/polymarket-rust/src/fast/signing_cache.rs` only if explicitly required;
- Corresponding tests.

## Implementation requirements

- First produce the source-to-signing trace against the frozen design;
- Do not change PBKDF2, AES-GCM, EIP-712, or external signing standards;
- Do not claim zeroization while Arc clones or external library copies remain alive;
- If changing code, use zeroize-aware buffers only within the project-controlled boundary;
- Track password clones as well as private-key buffers;
- Document the best-effort guarantee boundary.

## Required Evidence

- Trace of decrypt/unlock/sign/lock/drop;
- Arc clone lifetime analysis;
- TTL and sweep analysis;
- password clone analysis;
- wallet manager and hot wallet tests;
- no cryptographic standard changes.

## Stop condition

If a proposed change requires modifying external signing semantics or cannot establish the project-controlled zeroization boundary, stop and submit a Design Change Request.

---

# 4. Design Change Request protocol

A Design Change Request is mandatory if any of the following is discovered:

- frozen units are insufficient or contradictory;
- a second risk authority is needed;
- the state machine needs a new transition not specified in Design V2;
- persistence semantics are missing;
- exact order/fill identity cannot be established;
- a proto/schema change is required but not explicitly frozen;
- a threshold or trading behavior must change;
- implementation requires changing the authority, accounting, or reconciliation semantics.

The DCR must contain:

1. affected B item;
2. exact conflict/omission;
3. current code evidence;
4. proposed semantic change;
5. risk/downside;
6. migration/restart impact;
7. required tests;
8. rollback plan;
9. files that would change;
10. explicit human approval request.

When DCR is required: **stop implementation immediately**.

---

# 5. Verification and delivery

After each B item:

- run task-local tests;
- run the relevant full suite;
- inspect `git diff --name-only`;
- verify no forbidden file changed;
- report implementation gaps and unresolved risks.

After all authorized B items:

```bash
cd 'D:/WorkBuddyData/AI Weather'
C:/Users/32389/.workbuddy/binaries/python/envs/default/Scripts/python.exe -m pytest -q

export PROTOC=C:/Users/32389/tools/protoc/bin/protoc.exe
cd 'D:/WorkBuddyData/AI Weather/polymarket-rust/polymarket-rust'
cargo test
cargo clippy --all-targets
```

Do not use Docker claims unless Docker is actually available.

## Final output format

```text
Implemented B items:
Changed files:
Tests and real results:
Design Change Requests:
Implementation gaps:
Remaining risks:
Human review required:
```

Do not mark `VERIFIED` or `CLOSED`. Independent Hermes review remains required after implementation.
