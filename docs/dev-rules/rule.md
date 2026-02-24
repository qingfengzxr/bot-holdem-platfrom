1.模块化和可维护性是优先级最高的开发要求。
2.尽量坚持单一职责的原则
3.过长的代码文件，尽量按照功能拆分为多个文件
4.灵活的利用rust的模块的功能来组织我们的代码。
5.所有的仓库命令用mise组织
6.下面的这个是rs-poker仓库的一些供ai阅读开发的介绍

Commands
All commands via mise:

mise check               # Run all checks (format, clippy, tests, TOML lint)
mise check:test:nextest  # Run tests only
mise check:test:nextest test_name  # Run single test by name
mise check:test:docs     # Run doc tests
mise check:clippy        # Lint only
mise check:fmt           # Format check only

mise fix                 # Fix all formatting and lint issues

mise bench <target>      # Run benchmark (arena, rank, iter, parse, etc.)
mise fuzz <target>       # Run fuzz target for 60s (add --timeout 0 for infinite)
mise mutants             # Find missing test coverage
Fuzz Targets as Secondary Checks
After mise check passes, fuzz targets provide good secondary validation:

cfr_mixed_agents - tests CFRAgent and historian
replay_agent / multi_replay_agent - tests game replay
config_agent - tests config-driven agents
rank_seven - tests hand ranking
Architecture
rs-poker is a Rust poker library

Modules
core/: Card, Suit, Value, Hand, Rank, Deck. Bitset representations (CardBitSet/u64, PlayerBitSet/u16) for O(1) ops.
holdem/: StartingHand, range parsing ("KQo+", "99+"), Monte Carlo helpers, outs.
arena/: Multi-agent simulation (feature-gated):
GameState: Round state machine (preflop → flop → turn → river → showdown)
HoldemSimulationBuilder: Builder pattern for constructing simulations
Agent trait: Implement act(&self, &GameState) -> AgentAction; must be Clone
Historian trait: Event recording (VecHistorian, DirectoryHistorian, StatsTrackingHistorian)
AgentGenerator/HistorianGenerator: Factory traits for multi-simulation use
cfr/: Counterfactual Regret Minimization solver (arena allocation, nodes in Vec by index)
open_hand_history/: OHH format import/export
simulated_icm/: Tournament ICM calculations
Feature Flags
default = ["arena", "serde"]
arena                      # Multi-agent simulation
serde                      # JSON serialization
arena-test-util            # Testing helpers with approx comparisons
open-hand-history          # OHH format support
open-hand-history-test-util  # OHH testing helpers
Conventions
All clippy warnings denied: #![deny(clippy::all)]
Error handling via thiserror with domain-specific enums
Test utilities in test_util modules
Benchmarks in benches/, fuzzing in fuzz/
Bitset representations critical for performance
