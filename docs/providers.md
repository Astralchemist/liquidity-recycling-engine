# Providers: documentation review, 2026-10-02

This review covers official documentation for Binance (Spot and USDⓈ-M), Bybit V5, OKX v5 and Tiingo (crypto). It was read on 2026-10-02 to prepare the Phase 8 native public feeds and the Phase 10 demo adapters.

- Every fact comes from an official page; URLs are listed under [Sources](#sources).
- Items marked *observed* were checked against a live public endpoint, not taken from documentation.
- Items marked *unverified* could not be confirmed from an official page.
- Doc dates seen:
  - Binance Spot changelog: 2026-09-18
  - Binance derivatives changelog: 2026-09-28
  - Bybit V5 changelog: 2026-09-29
  - OKX changelog: 2026-09-30

This is an engineering summary, **not legal advice**. Read the terms yourself before opening accounts.

## Does adding them require direct contact?

**Short answer: not for market data or demo trading. Yes for maker rebates, and jurisdiction may block accounts entirely.**

| | Binance | Bybit | OKX | Tiingo |
|---|---|---|---|---|
| Public L2 market data | No key or account | No key; US and Mainland China IPs get 403 | `books`, `books5`, `bbo-tbt` and trades need no key. **`books-l2-tbt` and `books50-l2-tbt` need login plus VIP4.** | Account token (self-service) |
| API keys | Self-service. Requires 2FA, a deposit and identity verification. A key without an IP restriction can only read. | Self-service. KYC mandatory, 2FA; may be blocked for 48 h after registration. Keys without IP binding expire after 90 days. | Self-service; **KYC level 2 required to place live orders**. Keys with trade permission and no IP expire after 14 days of inactivity. | Self-service |
| Test/demo trading | Spot testnet via **GitHub login** (no Binance account). Futures demo needs an account but no KYC. | Testnet keys self-service. Demo Trading needs a **mainnet account**. | Demo keys self-service from an OKX account; demo is KYC-exempt. | n/a |
| Maker rebates | Spot Liquidity Program is automatic once weekly maker volume meets tier 1; a trial needs **email to mmprogram@binance.com**. Futures market-maker programme: **email with volume proof**. | Negative maker fees only in the market-maker programme: **email institutional_services@bybit.com**. Rate limits, cancel-on-disconnect and SBE feeds go through a client manager. | Negative maker fees from **VIP7** (volume or assets). Market-maker programme: **email Institutional@okx.com** (VIP2+). | n/a |
| Commercial or business use | Binance Vision dataset terms: CC BY-NC-SA, written licence for commercial use; scope versus live APIs is ambiguous. | Global API terms could not be read (rendered by JavaScript). | API Agreement §9.4: personal, non-commercial use; commercial use needs a data licence. | Any organisation must use the **Commercial** plan (self-serve upgrade). More than 2 developers or redistribution means **sales@tiingo.com**. |
| **Canada** | *Unverified*: search snippets list Ontario as restricted; the Terms page could not be read. | **Restricted** (Service-Restricted-Countries, 2026-09-01) | **Restricted** (Restricted Locations, 2026-07-08) | Not listed |

### What this means for the plan

1. **Phase 8 (native public feeds) needs no contact and no account.**
   - The one exception is OKX tick-by-tick L2, which needs VIP4. Start with OKX `books`: 400 levels, a snapshot then increments every 100 ms.
   - Use `bbo-tbt` (10 ms, public) for the touch.
2. **Phase 10 demo trading is self-service, but needs accounts.**
   - Binance Spot testnet needs only GitHub.
   - Bybit Demo needs a mainnet account; OKX demo keys come from an OKX account.
   - If you are resident in Canada, Bybit and OKX accounts, and therefore their demo trading, appear to be outside their terms. This is your decision; I can't determine your legal position.
3. **The specification's "maker economics" do not exist at retail tiers.** At entry level every venue charges a **positive** maker fee:
   - Binance Spot 0.100%, USDⓈ-M 0.020%
   - Bybit perps 0.020%
   - OKX perps 0.020%

   Rebates need programme acceptance or high volume. Phase 9 must model positive maker fees as the base case.
4. **Terms that bear on this project:**
   - **OKX API Agreement §8.1(b)** prohibits exploiting "latency arbitrage, or order matching logic in a manner not consistent with legitimate trading". This matters before ever trading on any cross-venue lead/lag finding.
   - **Tiingo Starter** forbids keeping Tiingo data in any persistent storage, including logs. Record Tiingo only on a paid plan, and never store per-tick Tiingo-minus-native differences next to native prices.

## Native book synchronisation rules (Phase 8 adapters)

These rules apply on every venue:

- Quantities are **absolute**, and size 0 deletes a level.
- Prices and sizes arrive as decimal strings. Parse them to integer ticks and lots with the venue's tick and step; never use floats.
- Each native depth message is an **atomic batch**. Apply it with the Phase 3 `apply_depth_batch`, and record it only after the versioned batch envelope exists. Recording format v1 cannot encode batches.

| | Binance Spot | Binance USDⓈ-M | Bybit (spot/linear) | OKX (`books`) |
|---|---|---|---|---|
| Stream | `wss://stream.binance.com:9443`, `@depth@100ms` (market-data only: `data-stream.binance.vision`) | `wss://fstream.binance.com/public`, `@depth@100ms` | `wss://stream.bybit.com/v5/public/{spot\|linear}`, `orderbook.{1\|50\|200\|1000}` | `wss://ws.okx.com/ws/v5/public` (port 443; 8443 ends 2026-10-31) |
| Snapshot | REST `GET /api/v3/depth?limit=5000` (weight 250) | REST `GET /fapi/v1/depth?limit=1000` | Pushed on subscribe; `u=1` means service restart, so reset. REST depth 1000 matches the 1000-level `u`. | Pushed (`action: snapshot`, `prevSeqId = -1`) |
| Sync start | Buffer; snapshot `lastUpdateId ≥ first U`; drop `u ≤ lastUpdateId`; first event must satisfy `U ≤ lastUpdateId+1 ≤ u` | Drop `u < lastUpdateId`; first event must satisfy `U ≤ lastUpdateId ≤ u` | First message is the snapshot | First message is the snapshot |
| Continuity | `U == prev_u + 1`; **`U > local+1` is a gap, resync** | **`pu == prev u`**, otherwise resync | `u == prev_u + 1` (documented for full depth, *observed* on all depths); ignore `u ≤ last`; drop if `seq` went backwards | **`prevSeqId == prev seqId`**. `seqId` may *decrease* after maintenance. Empty update with `prevSeqId == seqId` is a heartbeat. |
| Checksum | none | none | none | **Deprecated, always 0 since 2026-06-23: do not validate** |
| Keepalive | Server pings every 20 s; echo the payload | Server pings every 3 min | Send `{"op":"ping"}` every 20 s | Send text `ping` within 30 s; expect `pong` (code 4004 on idle) |
| Limits | 5 inbound msgs/s, 1024 streams per connection, 300 connects per 5 min per IP, 24 h lifetime | 10 msgs/s, 1024 streams, 24 h | Spot ≤10 args per subscribe; 500 connections per 5 min | 3 connections per s per IP; 480 subscriptions per hour per connection |
| Timestamps | ms; µs via `timeUnit=MICROSECOND` | ms (`E` event, `T` transaction) | ms: `ts` gateway, `cts` matching engine | ms `ts` |
| Trade aggressor | `m=true` means the buyer was maker, so the aggressor **sold** | aggTrade only (no raw trade stream) | `S` is the taker side | `side` is the taker side |
| Metadata | `exchangeInfo`: `PRICE_FILTER.tickSize` 0.01, `LOT_SIZE.stepSize` 0.00001, `NOTIONAL` min 5 | tickSize 0.10, stepSize 0.001; check `(price − minPrice) % tick` | linear tick 0.10, step 0.001; spot tick 0.1, basePrecision 0.000001 (paginate with `cursor`) | SWAP: `ctVal` 0.01 BTC, tick 0.1, lot 0.01 (**sizes are in contracts**); SPOT tick 0.1 |

The metadata values are BTCUSDT and BTC-USDT figures observed on 2026-10-02.

Adapter consequences for this engine:

- **OKX SWAP sizes are contracts.** Base quantity = `sz × ctVal`. That is a Phase 3 `UnitScale` conversion, so assert `ctMult = 1`, `ctType = linear`, `ctValCcy = BTC`.
- **Binance futures and OKX/Bybit linear are linear USDT perpetuals.** Spot is a different contract. The Phase 3 contract-equivalence check must not merge spot and perps into one consolidated book.
- **Retail Price Improvement (RPI) liquidity** is excluded from standard book feeds on all three exchanges. RPI prints can therefore appear outside the visible book; treat such prints as unmatched.
- **Record local monotonic receive time** next to every exchange timestamp. Lead/lag work (§28) must not use exchange timestamps from different venues as if synchronised.

## Fees (Phase 9 inputs)

| | Spot maker / taker (base) | Perp maker / taker (base) | Best published maker |
|---|---|---|---|
| Binance | 0.100% / 0.100% | 0.020% / 0.050% | Spot LP tier 4: −0.008%; futures LP rebates unverified for 2026 |
| Bybit | 0.100% / 0.100% | 0.020% / 0.055% | MM3: −0.0040% (perps), −0.0075% (spot) |
| OKX | 0.080% / 0.100% | 0.020% / 0.050% | VIP9: −0.005% |

Sign conventions differ. OKX's fee API reports a rebate as positive, while its fee website shows a rebate as negative. Normalise to one convention in the adapter.

## Tiingo as a reference feed

Tiingo is secondary only. It must never enter book state, never repair gaps and never seed snapshots.

- **Endpoint.** `wss://api.tiingo.com/crypto`, subscribe with `thresholdLevel: 2` for per-exchange top-of-book quotes plus trades.
  - Quote: `["Q", ticker, date, exchange, bidSize, bidPrice, midPrice, askSize, askPrice]`
  - Trade: `["T", ticker, date, exchange, size, price]`
  - Heartbeat about every 30 s.
- **No sequence numbers.** Gaps cannot be detected.
- **Timestamps.** Not stated whether they are exchange time or Tiingo receive time. Stamp local receive time ourselves.
- **Tiingo's own caveats.** Its deprecated REST top-of-book page says exchange data can be unreliable for building a best bid/ask, with wrong timestamps and out-of-order messages.
- **Use.** Compare only same-venue (`exchange`, `ticker`) against our native book. Flag divergence only when it persists, and keep aggregated statistics on the free plan.
- **Limits.** Free tier is 50 requests per hour, 1,000 per day, 1 GB per month, 500 symbols per month. The WebSocket "firehose" is included per the docs.

## Unverified or conflicting items

- **Binance.**
  - The Terms page's restricted locations could not be read.
  - Futures STP default: REST says `EXPIRE_MAKER`, the WebSocket API page says `NONE`.
  - The futures testnet WebSocket API host: `testnet.binancefuture.com` or `demo-*`.
  - Whether unrouted fstream URLs survive past 2026-04-23.
  - Current futures LP rebate tiers.
- **Bybit.**
  - `u` contiguity on non-full depths is observed but not documented.
  - Whether the 10-minute idle disconnect applies to public connections.
  - Global API Terms text; testnet KYC; batch size (10 or 20).
  - RPI access (the changelog conflicts with an error message).
- **OKX.**
  - The JSON resync procedure after a gap; resubscribing is inferred.
  - Whether `books` `ts` is engine or gateway time; whether demo public data mirrors production.
  - The rule for storing market data internally, as opposed to redistributing it.
- **Tiingo.**
  - The exchange identifier list and casing; whether timestamps are exchange or receive time.
  - Whether WebSocket traffic counts toward plan limits.
  - The pricing page renders the firehose for Starter inconsistently.

## Sources

**Binance**

- Spot WebSocket streams: https://github.com/binance/binance-spot-api-docs/blob/master/web-socket-streams.md
- Spot REST: https://github.com/binance/binance-spot-api-docs/blob/master/rest-api.md
- Market-data-only endpoints: https://github.com/binance/binance-spot-api-docs/blob/master/faqs/market_data_only.md
- Spot testnet: https://github.com/binance/binance-spot-api-docs/blob/master/testnet/general-info.md
- USDⓈ-M local order book: https://developers.binance.com/legacy-docs/derivatives/usds-margined-futures/websocket-market-streams/How-to-manage-a-local-order-book-correctly
- USDⓈ-M connection rules: https://developers.binance.com/legacy-docs/derivatives/usds-margined-futures/websocket-market-streams
- API key FAQ: https://www.binance.com/en/support/faq/detail/360002502072
- Futures market-maker programme: https://www.binance.com/en/support/faq/binance-futures-market-maker-program-b65fefd0fee84893ad946dc6f707dedc
- Binance Vision dataset terms: https://github.com/binance/binance-public-data/blob/master/TERMS_AND_CONDITIONS.md

**Bybit**

- WebSocket connection: https://bybit-exchange.github.io/docs/v5/ws/connect
- Order book topic: https://bybit-exchange.github.io/docs/v5/websocket/public/orderbook
- Full-depth order book: https://bybit-exchange.github.io/docs/v5/websocket/public/full-ob
- Demo Trading: https://bybit-exchange.github.io/docs/v5/demo
- Rate limits: https://bybit-exchange.github.io/docs/v5/rate-limit
- Service-restricted countries: https://www.bybit.com/en/help-center/article/Service-Restricted-Countries
- Market-maker incentive programme: https://www.bybit.com/en/help-center/article/Introduction-to-the-Market-Maker-Incentive-Program

**OKX**

- Order book channel: https://www.okx.com/docs-v5/en/#order-book-trading-market-data-ws-order-book-channel
- Changelog: https://www.okx.com/docs-v5/log_en/
- Risk and compliance disclosure: https://www.okx.com/help/risk-compliance-disclosure
- API Agreement: https://www.okx.com/help/okx-api-agreement

**Tiingo**

- Crypto WebSocket: https://www.tiingo.com/documentation/websockets/crypto
- Pricing: https://www.tiingo.com/about/pricing
- Terms of service: https://www.tiingo.com/tos
