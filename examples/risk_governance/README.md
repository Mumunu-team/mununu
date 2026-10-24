# Financial Risk-Governance Example

A pedagogical CTXDSL example that models the **protocol layer** of a
trading-desk risk-governance supervisor as two composed automata:

- A **drawdown response** state machine that must reach a kill-switch
  within bounded time when a critical breach occurs.
- An **instrument-validation gate** that any new execution instrument
  must traverse before being deployed live.

## Scope disclaimer

This example demonstrates a **property class** — discrete protocol gates
that compose into a governance supervisor. It is **not** a finding about
any specific firm, fund, product, or instrument, and the thresholds
(5%, 10%, 25%) are illustrative, not calibrated against any real risk
model. See the *Claims Integrity — Public Materials* section of
[`CLAUDE.md`](../../CLAUDE.md) for the rules that govern public claims.

The quantitative root cause that drives this kind of loss event in the
real world — tail behaviour of a derivative under volatility regime
shifts, premium decay outside backtest envelopes, etc. — is out of
mununu's scope. mununu verifies the protocol FSM that *consumes* such
signals, not the signal generator itself.

## Glossary

The informal explanation below uses trading-desk vocabulary. For
readers coming from a software / formal-methods background rather than
a markets background, the terms are summarised here. Definitions are
deliberately operational — what the term *means for the protocol*
rather than its full finance-theoretic content.

### Roles and venues

- **Trading desk** — a team inside a firm that trades a particular
  asset class or strategy. A firm typically has many desks (equities,
  rates, options, FX, etc.), each with its own runbook.
- **Risk team** — the desk-level or firm-level function that monitors
  loss exposure and is empowered to force position reductions when
  thresholds are crossed.
- **Research team** — the function that designs new trading
  strategies, backtests them, and proposes them for deployment.
- **Ops (operations) team** — the function that promotes approved
  strategies into the live trading environment.
- **Runbook** — a written, step-by-step procedure each team follows.
  In this example, each FSM models one runbook.
- **Cross-desk** — anything that spans more than one desk or team.
  The governance gap modelled here is *cross-desk*: it lives in the
  composition of two runbooks that each look correct in isolation.

### Position, book, P&L

- **Position** — the firm's current holding in a particular
  instrument. Long = owns it; short = owes it.
- **Book** — the full set of positions a desk holds. "The volatility
  book" is shorthand for "all the positions that desk runs."
- **P&L (profit and loss)** — the running gain or loss on a book,
  marked to current market prices. Live, continuous, and the signal
  the risk team watches.
- **Mark-to-market** — recomputing P&L from current market prices
  (rather than from purchase price). What makes P&L move second by
  second even when no trade has happened.
- **Drawdown** — the loss from the most recent peak P&L, expressed as
  a percentage. The state machine in this example uses drawdown
  thresholds (5%, 10%, critical) as the trigger for protocol
  transitions.
- **Flatten** — sell the book down to zero exposure. The
  kill-switch action. After flatten, the desk holds no position.
- **Deleveraging** — reducing position size (not necessarily to
  zero). `cut_50pct` is a deleverage step; `flatten` is the terminal
  case.
- **Kill-switch** — the protocol-level mechanism that forces a
  flatten when a critical threshold is crossed. Modelled here as the
  `CriticalBreach -flatten-> Halted` transition.

### Strategy lifecycle

- **Strategy / instrument** — a recipe for trading (an options
  strategy, an arbitrage rule, a market-making algorithm). New
  strategies are proposed, tested, and eventually deployed.
- **Backtest** — re-running the strategy against historical market
  data to estimate how it would have performed. Cheap, fast, but
  vulnerable to overfitting and regime-shift blindness.
- **Sim (simulation)** — running the strategy in a simulated trading
  environment, often with synthetic order flow, latency models, and
  market-impact assumptions. Stronger than backtest.
- **Eligible / promote to eligible** — the strategy has passed
  research and sim review and is queued for deployment. Not yet live.
- **Deploy live** — operations releases the strategy into the live
  trading environment, where it starts trading with real capital.
- **No-bypass workflow** — the structural property of
  `InstrumentValidator`: every strategy must pass through *every*
  validation step in order, with no shortcut.

### Risk vocabulary

- **Volatility** — the magnitude of price moves, often measured as
  annualised standard deviation. High volatility = large daily
  swings.
- **Volatility regime** — a period during which volatility behaves
  consistently (e.g. "low-vol regime" = months of small daily moves).
  A *regime shift* is when the market transitions abruptly to a
  different regime — the classical cause of multi-sigma loss events.
- **Multi-sigma loss** — a loss many standard deviations larger than
  the historical norm. By definition rare under a Gaussian model;
  empirically more common because real returns have fat tails.
- **Tail behaviour / tail risk** — the shape of the extreme end of
  the return distribution. Strategies that look profitable under
  normal conditions can have catastrophic tail risk.
- **Greeks** — sensitivities of an option's price to the things that
  move it (delta = sensitivity to the underlying, gamma = sensitivity
  of delta, vega = sensitivity to volatility, theta = sensitivity to
  time). Continuous quantities, out of mununu's scope.
- **Premium decay (theta)** — options lose value as expiry
  approaches. Short-option strategies *earn* premium decay; they are
  the ones most vulnerable to tail blowups.
- **Slippage** — the difference between the price you expected to
  trade at and the price you actually got. Larger during stressed
  markets and during a flatten.

### Specific products mentioned

- **Options** — contracts giving the right (not obligation) to buy
  or sell at a fixed price. "Selling options" earns premium up front
  in exchange for tail risk.
- **Variance swap** — a contract whose payoff depends on realised
  variance. "Selling variance" is structurally similar to selling
  options: small steady gains, occasional large losses.
- **VIX / inverse VIX ETP** — VIX is the equity-volatility index;
  inverse VIX ETPs (exchange-traded products) profit when volatility
  *falls*. The most famous blowup of this product class was
  XIV/SVXY in February 2018.

### Tooling and methodology

- **Monte Carlo backtester** — runs the strategy against
  stochastically generated price paths rather than (or in addition
  to) historical data. Stronger than a pure historical backtest.
- **TCA (Transaction Cost Analysis)** — post-trade analysis of
  execution quality, slippage, and market impact. Belongs in the
  execution layer, not the protocol layer.
- **Pre-trade gating** — checks run *before* a trade is allowed to
  fire (margin available? instrument eligible? kill-switch
  disengaged?). The `InstrumentValidator` workflow is a pre-trade
  gate.
- **Margin** — collateral posted to support a leveraged position.
  Margin calls and forced liquidations live in the same operational
  space as the kill-switch but on a different time scale.

### Formal-methods terms used informally below

- **FSM (finite-state machine) / LTS (labelled transition system)**
  — the formal object underneath each runbook. States, transitions,
  labels on transitions.
- **Composition** — combining two FSMs into one larger FSM whose
  states are pairs. *Asynchronous* composition lets them interleave;
  *synchronous* composition forces lockstep on shared labels.
- **Controllability** — for each label, whether the controller (desk)
  decides when it fires (controllable) or whether the environment
  (market) decides (uncontrollable).
- **Supervisor** — a derived controller that disables certain
  transitions in certain states so that a joint property holds. What
  `mununu context synth` produces.
- **Counterexample / witness trace** — when a property fails, the
  sequence of states and transitions that exhibits the failure.

## The problem, informally

Picture a trading desk on a quiet morning. Two things are happening in
parallel, on two different screens, run by two different teams:

1. **The risk team** watches a live P&L. A volatility regime change
   pushes one book into drawdown. A 5% threshold trips; then 10%; then
   the desk crosses what the policy doc calls "critical." The policy
   says: at critical, flatten the book — sell down the position to zero,
   stop new exposure.
2. **The research team**, oblivious to what risk is doing, has just
   finished backtesting a new options strategy. Sim looks good. They
   click "promote to eligible." Ops, also oblivious, sees an eligible
   instrument and clicks "deploy live" — the new strategy starts
   trading.

Each desk did exactly what its runbook said. The risk runbook says
*flatten on critical*. The ops runbook says *deploy eligible
instruments*. Neither runbook mentions the other. The result is a brand
new position being opened on the same desk that is supposedly
deleveraging, on a market regime that has already burned the existing
book. In the worst case the new instrument is *more sensitive* to the
move that triggered the kill-switch in the first place — the firm has
just doubled down at exactly the wrong moment.

This is not a model of derivative pricing. It is not a model of
correlation, of liquidity, of margin. It is a model of **how the
protocol is allowed to behave when two independent runbooks run on the
same firm at the same time** — and the assertion is that *no operator
should ever be able to deploy a new instrument while the kill-switch is
engaged*, irrespective of what either runbook says in isolation.

Real loss events that fit this shape are common in two flavours:

- **Cross-desk operational risk.** A new product is signed off through
  its own committee, but the firm-wide risk gate that should have
  blocked it during a drawdown was either out-of-process, latent, or
  manually overridable. The product launches into a stressed book.
- **Volatility-product blowups.** A book that sells volatility (short
  options, short variance swaps, inverse VIX ETPs, etc.) carries
  asymmetric drawdown risk: small daily gains for a long time, then a
  multi-sigma loss in hours. Policy says the desk should flatten as the
  loss accumulates, but the *protocol* — the actual sequencing of
  approvals, deploys, and kill-switches — has gaps that allow new
  exposure to be added during the unwind.

mununu cannot tell anyone whether a particular derivative is mispriced
or whether a volatility regime is about to break. It *can* prove,
exhaustively and mechanically, that the protocol around the desk does
or does not have the gap above. That is the entire scope of this
example.

## The problem, formally

We model the trading desk as the asynchronous composition of two
labelled transition systems. Each LTS captures one runbook; their
labels are disjoint, so by default each FSM advances independently of
the other. The joint property we want is a *safety* property over the
product state space.

### `DrawdownMonitor` — five states with `drawdown_pct` valuations

The risk runbook is captured as a deterministic FSM whose states carry
a `drawdown_pct` valuation so counterexample traces display the
protocol level directly:

| State            | `drawdown_pct` | Meaning                                          |
|------------------|----------------|--------------------------------------------------|
| `Normal`         | 0              | Initial. No drawdown signal active.              |
| `Watch`          | 5              | Crossed the 5% advisory threshold.               |
| `Reducing`       | 15             | Crossed 10%; controller may cut position.        |
| `CriticalBreach` | 25             | Crossed critical; only `flatten` reaches Halted. |
| `Halted`         | 25             | Kill-switch engaged.                             |

Environment-driven (uncontrollable) labels are the market signals the
desk does *not* get to pick: `dd_5`, `dd_10`, `dd_critical`, `recover`.
Controller-driven (controllable) labels are the policy actions the desk
*can* execute: `cut_50pct`, `flatten`, `reset_session`.

The interesting fragment is `CriticalBreach -flatten-> Halted` — once
the drawdown crosses the critical threshold, only the controller's
`flatten` action reaches `Halted`. The bounded-liveness property checks
that the controller can in fact force this from any reachable state.

### `InstrumentValidator` — linear gating workflow

Five states forming a no-bypass workflow:

```
Proposed -> Backtested -> SimValidated -> LiveEligible -> Live
```

No edge bypasses `SimValidated`, so the no-bypass property is
*structural* — it is true by construction of the FSM, not by virtue of
any controller policy. The validation steps (`backtest_pass`,
`sim_validate`, `mark_eligible`, `deploy_live`) are controllable;
failures (`backtest_fail`, `sim_fail`) are uncontrollable. This
reflects reality: the desk decides when to promote, but the simulator
decides whether the backtest passes.

### Composition

```
asynchronous governance_system {
    members [DrawdownMonitor, InstrumentValidator];
}
```

The two FSMs interleave freely (their label alphabets are disjoint),
producing a 5 × 5 = 25-state composite. The joint correctness property
is therefore a synthesis constraint, not a structural invariant — there
is nothing in the raw composition that prevents `deploy_live` from
firing while `DrawdownMonitor` is in `Halted`.

### Properties

- **`no_deadlock`** — `νX.(◇⊤ ∧ □X)`. Every reachable composite state
  has at least one outgoing transition. Sanity check.
- **`kill_switch_bounded_liveness`** — `νX.(((!CriticalBreach) ∨ μY.(Halted
  ∨ ◇Y)) ∧ □X)`. From any state where `CriticalBreach` holds, `Halted`
  is reachable. Realizable under `synth` because `flatten` is
  controllable and `CriticalBreach -flatten-> Halted` exists.
- **`no_deploy_when_halted`** — `νX.(((!Halted) ∨ [deploy_live]false) ∧
  □X)`. Pure structural safety: the composite state `Halted|LiveEligible`
  must be unreachable. Fails on the raw async composition (the
  structural winning region collapses to the `*|Live` states). The
  synth verdict is *unrealizable* — see the discussion below for why
  and what would change.

## CLI invocations and expected verdicts

From the repo root (substitute `cargo run -p mununu-cli --` for `mununu`
during development):

```bash
# Sanity: composition has no deadlocked states.
# Expect: 25/25 states satisfying, 1/1 initial satisfying.
mununu context eval examples/risk_governance/financial_governance.ctxdsl \
    --formula no_deadlock --automaton governance_system

# Bounded liveness on the kill-switch.
# Expect: 25/25 states satisfying. SOUNDNESS NOTE about alternation depth 2.
mununu context eval examples/risk_governance/financial_governance.ctxdsl \
    --formula kill_switch_bounded_liveness --automaton governance_system

# Joint safety on the raw composition.
# Expect: 5/25 states satisfying (only *|Live states), 0/1 initial.
# The witness for non-satisfaction is Halted|LiveEligible — reachable
# whenever the controller flattens while the validator is in LiveEligible.
mununu context eval examples/risk_governance/financial_governance.ctxdsl \
    --formula no_deploy_when_halted --automaton governance_system

# Realizable synthesis for the kill-switch.
# Expect: realizable, 25/25 retained, projection-mode controller.
mununu context synth examples/risk_governance/financial_governance.ctxdsl \
    --formula kill_switch_bounded_liveness --automaton governance_system

# Unrealizable synthesis for the joint safety on the unrefined model.
# Expect: unrealizable, with Normal|Proposed as the violating initial.
# This is informative, not a tool bug — see "Why synth fails" below.
mununu context synth examples/risk_governance/financial_governance.ctxdsl \
    --formula no_deploy_when_halted --automaton governance_system
```

## Why `no_deploy_when_halted` is unrealizable here

mununu's synth extracts a strategy from the *eval winning region* — the
set of states from which the property is structurally invariant. The
winning region for this formula is just `{*|Live}`, because every other
state has at least one path (env-driven on drawdown plus controller-driven
on the validator) that reaches `Halted|LiveEligible`. Since the initial
state `Normal|Proposed` is not in `{*|Live}`, synth reports unrealizable.

The unrealizability verdict is itself a finding: it says the governance
protocol as written cannot jointly satisfy "flatten on critical
drawdown" *and* "never deploy under a kill-switch" *without some
additional mechanism that the protocol must explicitly include*.

## Mitigations

The four mitigations below are not alternatives — they are layers, in
order of strength. A serious deployment uses all four. The first two
are model-side: they change what the protocol *can* express. The third
is a synthesis-side discipline. The fourth is an operational practice
that wraps the whole pipeline.

### 1. Enrich the model with a controller-driven downgrade

Add an edge `LiveEligible -suspend_eligibility-> SimValidated` (or a
fresh `Quarantined` state) where `suspend_eligibility` is controllable.
The controller can then move the validator out of `LiveEligible` before
flattening, keeping `Halted|LiveEligible` unreachable along the chosen
path. The structural winning region grows to include the initial state,
and `mununu context synth` becomes realizable.

This is the most direct fix — it matches what a real desk would do
(suspend deploys when the kill-switch arms), and it makes the
*supervisor* the explicit mechanism that wires the two runbooks
together rather than relying on the absence of bad transitions.

### 2. Use a turn-based, game-aware encoding

The TLSF/AIGER adapter emits formulas of the form
`νX.(φ ∧ [(ctrl=Controllable)]X)` over a product that carries a turn
bit, so the modal box ranges only over the controller's moves at
controller turns and only over the environment's moves at environment
turns. See
[`crates/mununu-core/src/adapter/emit.rs`](../../crates/mununu-core/src/adapter/emit.rs)
(the `GAME_BOX` constant and `emit_game_formulas`) for the canonical
pattern.

This is the right encoding when the underlying problem genuinely is a
*game* — environment and controller alternate, and the controller's
ability to react after each environment move is part of the
specification. It makes liveness obligations like "the kill-switch
fires within N controller turns of the breach" expressible directly.

### 3. Synthesize a supervisor and ship the supervisor, not the protocol

Once the model is realizable, `mununu context synth` does not just
verify — it emits a controller that *disables* the offending
transitions in the offending states. In our case the synthesized
supervisor will disable `deploy_live` in every composite state where
`DrawdownMonitor` is in `Halted`. That supervisor is a CLTS in its own
right, exportable as native CTXDSL or (for compatible adapters) as
XState / SystemVerilog, and it is what the running system
should consult — not the underlying validator FSM in isolation.

The discipline is: the runbook is the *spec*; the synthesized
supervisor is the *implementation*. Operators do not ship the
unsupervised protocol.

### 4. Distinguish the protocol layer from the quantitative layer

mununu verifies the discrete FSM, not the continuous P&L. Mitigations
1–3 only help if the *signals* the protocol consumes are themselves
sound: a `dd_critical` event must actually fire when the book crosses
the critical threshold; an `mark_eligible` event must actually require
the backtest to have hit pre-registered targets, not be hand-flipped by
an operator.

That side is out of mununu's scope, and the protocol guarantees only
hold *modulo the signal layer*. Mitigations on the quantitative side
include probabilistic model checking (PRISM, Storm), Monte Carlo
backtesters, scenario stress tests, and pre-trade risk simulators. The
right pipeline is: *quantitative tools produce the signals;
mununu proves the protocol that consumes them is sound.*

## What mununu can actually express here

This example only uses a few mununu primitives. The full surface is
larger, and most of it is directly relevant to governance modelling.
The list below is non-exhaustive but covers the primitives a
risk-protocol model is most likely to reach for.

### Compositionality

- **Asynchronous interleaving** — `asynchronous { members [...] }`. Used
  here. Independent runbooks; disjoint alphabets.
- **Synchronous composition** — `synchronous { members [...] }`. Two
  FSMs that *must* take the same labelled transition in lockstep.
  Useful for tightly coupled controllers (e.g. a position-keeping
  service that must move in lockstep with a margin engine).
- **Superset composition** — partial synchronization on a chosen label
  subset. Useful when two desks share a few coordination events
  (`market_open`, `eod_close`) but otherwise run independently.

### Controllability and the game-theoretic layer

- **Per-label controllability** — `controllable { ... }` /
  `internal { ... }`. Distinguishes what the desk *chooses* (deploy,
  flatten, cut) from what the desk *observes* (drawdown signals,
  backtest results, market events). Used here.
- **Modal guards with a `ctrl` axis** — `[(ctrl = controllable)] φ` and
  `[(ctrl = environment)] φ` let you say "every controller move
  preserves φ" or "every environment move preserves φ" separately. The
  turn-based encoding in the TLSF/AIGER adapter is built on this.

### State-level structure

- **State valuations** — `state X { valuations { key = value; ... } }`.
  Used here for `drawdown_pct`, `phase`, `kill_switch_pending`, etc.
  Counterexample traces display these directly, so a violation prints
  as `drawdown_pct=25, kill_switch_pending=1 | Proposed` rather than as
  opaque integer state IDs.
- **State-level predicates** — `predicates { predicate p = state S }`
  for Kripke-style atomic propositions referenced inside mu-calculus
  formulas. Predicates also appear in modal guards as `req_cur`,
  `forb_cur`, `req_next`, `forb_next`.

### Transition-level structure

- **Multi-label transitions** — one edge can carry several semantic
  labels (`on label a, label b`). Useful when a single market event
  carries multiple tags (e.g. controllability class + signal name +
  payload kind).
- **Self-loops as soak states** — `transition CriticalBreach ->
  CriticalBreach on label dd_critical` (used here). Models that the
  environment can keep re-asserting the critical signal indefinitely.

### Property classes

- **Safety** (`νX.(φ ∧ □X)`) — "φ holds in every reachable state."
  `no_deploy_when_halted` is of this form.
- **Bounded liveness** (`νX.(ψ ∨ μY.(φ ∨ ◇Y)) ∧ □X`) — "from every
  reachable state where ψ holds, φ is reachable in finitely many
  steps." `kill_switch_bounded_liveness` is of this form.
- **Sanity / no-deadlock** (`νX.(◇⊤ ∧ □X)`) — "every reachable state
  has at least one outgoing transition." Used here as a smoke test.
- **Reachability** (`μX.(φ ∨ ◇X)`) — "φ is reachable." A common pattern
  for confirming that desired goal states (e.g. `Halted`) are
  reachable at all under the modelled environment.
- **Step-bounded modalities** — `[(steps = 3)] φ` constrains the modal
  box to paths of bounded length. Useful for "the kill-switch must
  engage within 3 controller turns of `CriticalBreach`."

### Synthesis modes

- **Projection** (default) — keeps all transitions between winning
  states. The retained sub-CLTS *is* the winning region.
- **Functional** (`--extract-strategy`) — picks one controllable
  transition per state, the most progressive under fixpoint rank. A
  deterministic strategy suitable for compilation into a runnable
  supervisor.
- **Permissive** — keeps all non-regressive controllable transitions.
  The maximally permissive supervisor (Ramadge-Wonham canonical).
  Composable with other supervisors when several governance layers
  must be combined.

### What mununu deliberately does NOT model

Worth being explicit about, because every one of these matters for a
real desk:

- **Continuous P&L dynamics**. Premium decay, mark-to-market moves,
  Greek exposures, slippage — none of these are in the discrete CLTS.
- **Stochastic / distributional behaviour**. Volatility regime shifts,
  tail-decay correlations, fat-tail option pricing — these need a
  probabilistic model checker (PRISM, Storm) or a Monte Carlo
  backtester.
- **Calibrated thresholds**. The drawdown percentages displayed via
  state valuations are illustrative and have no relationship to any
  real risk policy.
- **Real-time guarantees**. "Bounded liveness" here means *bounded in
  protocol rounds*, not bounded in wall-clock time. A timed-automaton
  tool (UPPAAL) is the right complement when wall-clock bounds matter.
- **Adversarial market microstructure**. Order-book reactions to a
  flatten event, market-impact feedback, slippage during the unwind —
  out of scope; this is the realm of execution simulators and TCA.

For the quantitative side, mununu composes naturally with external
tools: quantitative analysis produces the threshold values and the
controllability classification, and mununu proves the protocol FSM
that consumes them is sound.

## Agentic design: making mununu part of the development loop

The loss events that motivate this example are not, in the end, caused
by missing math. They are caused by *protocol gaps that were never
verified*. The reason they are never verified is that no human reliably
re-checks a 25-state cross-desk composition every time a new runbook
edge is added. An agentic development loop is the right place to fix
this — but only if the agent treats mununu as an *oracle*, not as a
post-hoc commentator.

The recommended pattern below applies whenever an LLM-based agent,
copilot, or automated workflow is allowed to modify a governance
protocol (a runbook, a state machine, a policy YAML, a kill-switch
rule). It is the same pattern the agentic examples under
[`examples/agentic/`](../agentic/) use for MCP authorization gates and
multi-agent handoff protocols.

### Why a human-only review is not enough

A human review of a governance change typically catches what is wrong
on the changed edge. It rarely catches what is wrong in the
*composition* of the changed edge with every other runbook on the
desk. The state space grows multiplicatively: two runbooks of five
states each compose into 25; three compose into 125; a real firm has
dozens. No human reliably enumerates 125 reachable states to confirm
that none of them violate a joint safety property after a one-line
change.

Mununu does enumerate them. Mechanically, exhaustively, and in
milliseconds. The agentic loop's job is to make sure it runs.

### The verification-first loop

The discipline is the same as the "Verification-first workflow" rule
in [`CLAUDE.md`](../../CLAUDE.md): **the model is the oracle, not
human reasoning about the code.** Adapted to governance protocols:

```
1. Agent receives a change request
   ("add a fast-track promotion path for hedging instruments").

2. Agent updates the CTXDSL spec
   (new state, new transitions, new controllability classification).

3. Agent runs `mununu context summarize` to confirm the change parsed
   and produced the expected automaton.

4. Agent runs `mununu context eval` on every safety and liveness
   property over every composition the changed automaton participates
   in. NOT just the one the change "touches" — every one.

5. Agent runs `mununu context synth` on every property whose
   supervisor is deployed. If any becomes unrealizable, the change
   has introduced a structural gap.

6. Only if every property still verifies and every supervisor remains
   realizable does the agent open the change for human review.

7. The human review reads the formal verdicts, not the diff alone.
```

Step 4 is the load-bearing one. The agent must NOT pre-conclude that a
property is unaffected because the changed edge "doesn't look like it
touches that property." Composition makes everything potentially
relevant. Run the check; let the tool speak.

### Where mununu sits among the other tools an agent should call

mununu is one verification tool. A serious agentic loop calls several,
and uses each for what it is designed for. A non-exhaustive sketch:

| Concern                                  | Tool class                                   | Example                              |
|------------------------------------------|----------------------------------------------|--------------------------------------|
| Discrete protocol / cross-desk gating    | mununu (this tool)                           | `mununu context eval/synth`          |
| Probabilistic safety (P[loss > X] < ε)   | Probabilistic model checker                  | PRISM, Storm                         |
| Wall-clock / latency bounds              | Timed-automaton checker                      | UPPAAL                               |
| Continuous P&L / Greeks / pricing        | Quantitative library / Monte Carlo           | QuantLib, in-house MC                |
| Backtest / regime-shift performance      | Historical backtester                        | In-house, vectorbt, etc.             |
| Execution / market-impact simulation     | Execution simulator / TCA                    | In-house, Optiver-style sims         |
| Code-level correctness of the runbook    | Type checker, linter, unit/integration tests | rustc, mypy, pytest, etc.            |
| Source-to-protocol fidelity              | Source extraction + mununu                   | `mununu extraction` + ast_extract    |
| Operational telemetry / runtime drift    | Observability stack                          | Grafana, alerts, SIEM                |

The agent's job is to know which concern a proposed change touches and
to call the *right* tool first. A change to a drawdown threshold is a
*quantitative* change — the agent should call the backtester and the
probabilistic checker. A change to *who can promote an instrument
under what conditions* is a *protocol* change — the agent should call
mununu. A change that does both should call both.

### Making mununu invocations part of the agent's tool surface

Concretely, an agent that owns governance code should have mununu
wired in as a first-class tool, not invoked ad-hoc through the shell.
Three layers are useful:

- **Pre-flight summary** — `mununu context summarize` on the updated
  spec, to confirm the agent's edits parsed and produced the expected
  automaton shape. Cheap, catches mechanical errors before any
  reasoning happens. Source of truth:
  [`/api/v1/context/summarize`](../../crates/mununu-core/src/api/handlers.rs).
- **Property panel** — `mununu context eval` for every named formula
  in the spec, with the result displayed as a verdict table next to
  the diff. This is the *body* of the change review: a green table
  means the change preserves every named property; a red row points
  directly at the broken property and the counterexample trace.
- **Synthesis panel** — `mununu context synth` for every deployed
  supervisor. Realizable / unrealizable is the deploy-gate. An
  unrealizable verdict blocks merge; the agent must either change the
  model (mitigation 1) or change the property (downgrade the policy
  intentionally and document it).

The HTTP API surface
([`/api/v1/context/eval`](../../crates/mununu-core/src/api/handlers.rs),
`/api/v1/context/synth`, `/api/v1/context/import`,
`/api/v1/context/summarize`) is what the agent calls. The CLI is what a
human runs from a terminal. The two are equivalent — they share the
same realization pipeline — so the agent's verdict is reproducible by
the human reviewer with one command.

### Counterexample traces as feedback to the agent

When `mununu context eval` finds a state where a property fails, it
emits a trace. The trace carries the `valuations` set on each state, so
it reads as a human-legible scenario:

```
Trace witness for !no_deploy_when_halted:
  step 0: drawdown_pct=0, phase=normal | Proposed
  step 1: drawdown_pct=5, phase=watch | Proposed   (env: dd_5)
  step 2: drawdown_pct=5, phase=watch | Backtested (ctrl: backtest_pass)
  step 3: ...
  step k: drawdown_pct=25, kill_switch_active=1 | LiveEligible
          ──> deploy_live is enabled here. Property violated.
```

That trace is the agent's input for the *next* iteration: it states
exactly which sequence of environment moves and controller moves
reaches the bad state. The agent does not have to *guess* what the
property is complaining about — the trace tells it. A good agentic
loop feeds the trace back into its own context, proposes a model
amendment (typically along mitigation 1 lines), re-runs the check, and
iterates until verification passes.

This is the same pattern the SystemVerilog and extraction adapters use
for RTL bugs and source-code vulnerabilities: trace as ground truth,
agent as iteration driver, mununu as oracle.

### What the agent is NOT allowed to do

Mirroring the CLAUDE.md rules:

- **Do not declare a property holds without running the check.**
  Reasoning about the diff is not verification; only running the
  formal evaluator is.
- **Do not silently weaken the property to make verification pass.**
  Property weakening is a *governance decision* and requires explicit
  sign-off, not a quiet edit.
- **Do not skip composition.** A property over `DrawdownMonitor` alone
  is not a property over `governance_system`. The check that matters
  is the joint one.
- **Do not extract claims from the model and present them as findings
  about the real desk.** The model is a *protocol abstraction*. A
  violation in the model is a *structural gap in the protocol*, not a
  loss event in the firm. Translation between the two is human
  judgment, not the agent's output.

### Bringing it together

The example in this directory is small enough to verify by reading
it. Real governance protocols are not. The shape of the loss events
that motivate this example is consistent: independent runbooks, a
joint state that no single team owns, and a change to one runbook that
silently invalidates a property nobody re-checked.

An agentic development loop that wires mununu in at the *change*
boundary — not at the audit boundary, not at the incident-review
boundary — is the difference between catching the gap before the
deploy and reading about it in the post-mortem. The tooling is here.
The remaining work is operational: wire it in, treat it as the oracle,
ship the supervisor, and re-run the loop on every change.
