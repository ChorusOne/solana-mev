# Solana-MEV

Solana-MEV is a modification of the [upstream Solana validator][upstream] that
handles certain MEV opportunities right in the banking stage of the validator.
Running this validator instead of the upstream one can generate a small amount
of additional income.

**Warning:** This is a proof of concept. [Chorus One][c1] has used it in
production for several months, but it is not at a level of polish where it is a
drop-in replacement for the upstream validator.

[upstream]: https://github.com/solana-labs/solana
[c1]:       https://chorus.one/

## Further reading and other media

 * [Decentralizing MEV: An Alternative to Block-Building Marketplaces][talk],
   a talk by Thalita Franklin at Breakpoint 2022.
 * [Breaking Bots: MEV on Solana and how to prevent frontrunning, spam attacks
   and centralization][breaking-bots], whitepaper by Thalita Franklin, Enrique
   Fynn, Umberto Natale, and Ruud van Asseldonk.
 * [Arbitrage as a Convex optimization problem][notion_page], a document by
   Umberto Natale describing the mathematical framework used to detect if an
   arbitrage exists.

[talk]: https://www.youtube.com/watch?v=nTEnpuDHz3w&t=6198s
[notion_page]: https://www.notion.so/chorusone/Arbitrage-as-a-Convex-optimization-problem-f2490665033f41b6b6d41cfd5196acae
[breaking-bots]: https://chorus.one/products/breaking-bots/

## Status

Chorus One has developed this prototype as a proof of concept. Currently it is
able to take arbitrage opportunities on Orca non-concentrated-liquidity pools.
However, with the current trading volume on Solana, developing Solana-MEV is not
sustaintable. Even if we were to cover the majority of Solana AMMs, as of
December 2022, a validator with ~1% of leader slots would extract only a few
dollars per day.

At this point, Chorus One does not plan to actively invest resources in
developing Solana-MEV further. We do hope that our prototype can serve as
evidence that a centralized marketplace is not a technical prerequisite for MEV
extraction, and we hope to get a discussion started around possible MEV
architectures on Solana to ensure that MEV benefits the entire community.

## Details

We are interested making arbitrages on Automated Market Maker (AMM) pools. When
we are a validator during block production, we look at every transaction to see
if the program id is one of the [configured](#configuration) known program ids.
If a transaction interacts with a known program id, we get all pools balances
before and after the transaction executed and log this information. Futhermore,
we scan the pool for arbitrage opportunities only
**after** the transaction is executed, in that way we forgo making sandwich
arbitrage transactions, i.e. acting in between the user's transactions.

Our strategy for arbitraging is checking for every
[configured](#configuration) path that start and finish at the same token if
there could be a transaction that generates profit.

* We introduce a config file that statically configures all cycles to watch for
   arbitrage opportunities. The config file is parsed and injected into the
   banking stage when MEV is enabled.
 * In `BankingStage::process_and_record_transactions`, we introduce an
   additional output: a single optional transaction, that should extract the MEV
   created by the transaction batch.
 * At the call site, `BankingStage::process_transactions`, if an MEV transaction
   was produced, we execute it.
 * `runtime/src/mev.rs` and `arbitrage.rs` contain methods that given a set of
   AMM pools, compute optimal input amount that maximizes profit. When the
   profit is smaller than the transaction fee, or even negative, we bail out.

Solana transactions are organized in batches called **Entries** that can execute
in parallel. Accounts in these entries can be referenced only once for
write-access and multiple times for read-access. Due to this fact, when we
encounter an arbitrage opportunity, we create a new Entry just for that
opportunity, this Entry ought to be executed immediately after the transactions
that resulted in the arbitrage, but it might happen that the arbitrage
transactions are not executed atomically after we spotted the arbitrage, see
more details in the following [limitations](#limitations) section.

## Limitations

We have some limitations when executing arbitrage transactions. The main one is
that we don't lock accounts in-between entries and it might happen that a worker
thread executes other entries in-between the entry produced for arbitrage, this
can lead to an incorrect behavior of the program. This issue is remedied by
defining all *minimum_output* of the arbitrage instructions (except the first)
as the input of the previous instruction. As an extra guarantee that the
transaction produces a profitable transaction, we check that the transaction is
profitable. Transactions that execute but fail are not included in the block.

We are limited to a maximum of three instructions per transaction, this is due
to Solana's limitations on the transaction's length, one could extend an
arbitrage to spawn over multiple sequential transactions to circumvent the
limitation.

## Comparison to alternatives

Compared to [jito-solana][jito-solana], Solana-MEV differs in a few key aspects:

 * **No central server.** Solana-MEV does not introduce new connections to
   third party servers, everything happens inside the validator.
 * **No mempool.** Solana-MEV does not buffer transactions. It inserts its own
   transactions in between user transactions, but it does not change the way in
   which user transactions are processed. This also means that Solana-MEV has
   virtually zero latency impact compared to Jito, which introduces several
   additional network hops in the transaction processing path.
 * **No transaction reordering.** Solana-MEV processes transactions in the same
   order as upstream Solana. It does not buffer transactions, so it has no way
   to reorder; it only inserts its own transactions in between user
   transactions.
 * **Built-in searcher.** Solana-MEV does not rely on external searchers for
   identifying MEV opportunities, it has a few basic strategies built-in. A
   limitation of this is that the strategies are not as advanced as those of a
   dedicated searcher, and Solana-MEV cannot respond as quickly to changes in
   the ecosystem (e.g. the launch of a new AMM).

Despite the differences, Solana-MEV and Jito are not incompatible, they are
complementary. Jito’s patches stream groups of entries (parts of a block) to the
validator, while Solana-MEV generates those internally based on the ones it saw
before. There is no fundamental technical barrier to combining the two sources,
however it is unclear what the marginal benefit is.

[jito-solana]: https://github.com/jito-foundation/jito-solana

## Reward distribution

The MEV module generates transactions that increase the balance of the SPL token
accounts owned by the _MEV Authority_ (see also the configuration section
below). Currently no mechanism is implemented to share those proceeds further.
In the case of a staking pool such as [Lido][lido], one simple way to share the
rewards would be to transfer any excess balance to the pool’s reserve.

[lido]: https://solana.lido.fi/

## Configuration

MEV extraction is enabled by providing the `--mev-config-path` command-line
option to `solana-validator`. Without this option, the validator will run as
usual. `--mev-config-path` should point to a TOML file with the following
schema:

```toml
# File to log details about MEV opportunities and AMM pools to.
log_path = '/path/to/mev.log'

# Programs to watch for interactions. After a user transaction interacts with
# one of these programs, we check for MEV opportunities afterwards.
watched_programs = [
  # Orca Swap v1
  'DjVE6JNiYqPL2QXyCUUh8rNjHrbz9hXHNYt99MQ59qw1',
  # Orca Swap v2
  '9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP',
]

# Path to the keypair of the "MEV Authority". This address is the owner of all
# SPL token accounts that are involved in MEV extraction, and it signs all
# transactions generated by the MEV module. For example, if there exists a
# triangular opportunity between the pools USDC/stSOL, stSOL/stETH, stETH/USDC,
# then this address should have an associated token account for USDC, stSOL,
# and stETH. This key is optional, if not provided, we only monitor for
# opportunities but don't extract.
user_authority_path = '/path/to/keypair.json'

[minimum_profit]
# Per token mint address, the minimum profit before we generate a transaction.
# This is to ensure that we don’t execute transactions whose profit is lower
# than the cost of the transaction fees. Note that because we only execute
# transactions when the validator itself is leading, we pay the fee to
# ourselves. However, because half of the fee is burned, we still need a mimum
# of half the transaction fee (5,000 lamports currently). The number is in the
# smallest unit of the token (e.g. lamports for SOL, 1e-6 USDC for USDC).
"So11111111111111111111111111111111111111112" = 2501  # 0.000_002_501 SOL
"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" = 101  # 0.000_101 USDC

# Next are the paths that we want to consider. A path is a sequence of Orca
# pools that should form a cycle. Note, due to the transaction size limit on
# Solana, it is generally not possible to use cycles of more than three hops,
# because they would need to reference too many accounts.
[[mev_path]]
name = "USDC->wstETH->stSOL->USDC"
path = [
    { pool = "v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG", direction = "BtoA" },
    { pool = "B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy", direction = "BtoA" },
    { pool = "EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL", direction = "AtoB" },
]

# For every Orca pool involved, we also need to specify its details.
[[orca_account]]
_id = "stSOL/SOL"
address = "71zvJycCiY2JRRwKr27oiu48mFzrstCoP6riGEyCyEB2"
pool_a_account = "HQ2XUmQefvBdpN8nseBSWNP2D1crncodLL73AWnYBiSy"
pool_b_account = "8y8X4JuZn1MckRo5J6rirpr2Dxj1RKQshj7VzuX6dMUw"
pool_mint = "4jjQSgFx33DUb1a7pgPsi3FbtZXDQ94b6QywjNK3NtZw"
pool_fee = "7nxYhYUaD7og4rYce263CCPh9pPTnGixfBtQrXE7UUvZ"

# If we want to also extract MEV and not only monitor for opportunities, we also
# need to provide the addresses of SPL associated token accounts, owned by the
# MEV authority defined earlier, for token A and token B. These are called
# "source" and "destination" respectively, though the roles can be reversed if
# the pool is used with the BtoA swap direction.
source = "..."
destination = "..."
```

## Future work

 * For technical reasons, inserting the MEV-extracting `Entry` currently does
   not happen atomically — it is not guaranteed that the entry lands directly
   after the one that created the opportunity. Doing so is possible, but
   requires more invasive changes to the codebase that would make the diff more
   difficult to maintain. Solana-MEV _does_ ensure that it does not include
   transactions that would make a loss, nor transactions that fail.
 * Currently Solana-MEV only observes Orca non-concentrated-liquidity pools,
   a logical next step would be to watch concentrated liquidity pools as well.

## License

Our modifications are licensed under the Apache 2.0 license like the original
Solana validator. As stated in the license, Chorus One is not liable for damages
that result from using this software.
