# Auto-Batching Embedding Proxy

A tiny Rust proxy that batches high-QPS embedding requests before forwarding them
to [Hugging Face Text Embeddings Inference (TEI)](https://github.com/huggingface/text-embeddings-inference). It keeps
per-request latency low (time-based flushing) while maximizing upstream throughput (size-based batching + bounded
concurrency).

* **Proxy API:** `POST /embed` with `{ "input": "..." }` → `{ "embedding": [...] }`
* **Upstream (TEI) API:** `POST /embed` with `{ "inputs": ["...", ...] }`

---

### Requirements

* Docker + docker compose
* NVIDIA GPU & drivers (the compose uses `gpus: all`)
  and [nvidia-container-toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html)

### Run

```bash
docker compose up --build
```

* Proxy: [http://localhost:3000](http://localhost:3000)
* TEI:   [http://localhost:8080](http://localhost:8080)

---

## Configuration (env)

| Variable            | What it does                             | Example / Default |
|---------------------|------------------------------------------|-------------------|
| `TEI_URL`           | TEI base URL (`http://tei:80`)           | **required**      |
| `MAX_WAIT_TIME_MS`  | Max time to wait to fill a batch         | `8`               |
| `MAX_BATCH_SIZE`    | Batch size cap per flush                 | `32`              |
| `BATCH_CONCURRENCY` | # of concurrent upstream calls (permits) | `8`               |
| `QUEUE_CAP`         | Bounded queue capacity (backpressure)    | `2048`            |
| `BIND_ADDR`         | Proxy listen address                     | `0.0.0.0:3000`    |

> Tip: Lower `MAX_WAIT_TIME_MS` for lower tail latency; raise `MAX_BATCH_SIZE`/`BATCH_CONCURRENCY` for throughput (as
> long as TEI can handle it).

---

## API

### Health

```
GET /health
200 OK
ok
```

### Embed (proxy)

```
POST /embed
Content-Type: application/json

{ "input": "hello world" }
```

Response:

```json
{ "embedding": [0.0123, -0.0456, ...] }
```

> The proxy translates this to TEI’s `{ "inputs": ["..."] }` under the hood and fans out the result.

---

## Benchmark tool

A tiny closed-loop load generator is included (Rust). It can hit either the **proxy** or the **native** TEI endpoint to
compare:

```bash
# Build & help
cargo run --release --bin bench -- --help
Usage: bench [OPTIONS] --requests <REQUESTS> --concurrency <CONCURRENCY> --service <SERVICE>

Options:
  -r, --requests <REQUESTS>        
  -c, --concurrency <CONCURRENCY>  
  -s, --service <SERVICE>          [possible values: proxy, native]
  -t, --tokens <TOKENS>            Approximate tokens per request (repeated word tokens). Use realistic sizes like 32, 128, 256, 512… [default: 128]
  -h, --help                       Print help
  -V, --version                    Print version

# Example: hit TEI directly
cargo run --release --bin bench -- -r 20000 -c 2048 -s native

# Example: hit the proxy
cargo run --release --bin bench -- -r 20000 -c 2048 -s proxy
```

* **throughput** – requests per second (closed-loop)
* **successes / fail** – HTTP 2xx vs non-2xx / client errors
* **latency p50/p95/p99** – end-to-end per request

> The bench drains bodies to keep connections hot, uses a shared `reqwest::Client`, and bounds in-flight with a
> semaphore (`-c`).

## Results

###### XPS laptop

|            Setup | Total req | Tokens | Concurrency | Throughput (req/s) | Success | Fail | p50 (ms) | p95 (ms) | p99 (ms) |
|-----------------:|----------:|-------:|------------:|-------------------:|--------:|-----:|---------:|---------:|---------:|
| **TEI (native)** |    20,000 |      2 |       2,048 |        **8,528.5** |  20,000 |    0 |   188.05 |   505.42 | 1,640.42 |
|        **Proxy** |    20,000 |      2 |       2,048 |       **11,232.0** |  20,000 |    0 |   170.30 |   212.57 |   239.36 |
| **TEI (native)** |    20,000 |     16 |       2,048 |        **3,496.7** |  20,000 |    0 |   568.58 |   598.85 | 1,742.50 |
|        **Proxy** |    20,000 |     16 |       2,048 |        **3,579.8** |  20,000 |    0 |   561.90 |   571.45 |   594.03 |
| **TEI (native)** |    10,000 |     32 |       2,048 |        **1,926.7** |  10,000 |    0 | 1,035.65 | 1,906.64 | 2,222.62 |
|        **Proxy** |    10,000 |     32 |       2,048 |        **1,930.0** |  10,000 |    0 | 1,039.08 | 1,063.27 | 1,154.44 |

---

## How batching works

* One accumulator task reads from a channel.
* It **fast-drains** queued requests, then **awaits** “one more item **or** deadline” (whichever comes first).
* When a batch is ready, it spawns a flush task; a **semaphore** bounds concurrent upstream TEI calls.
* Each request gets a `oneshot` to deliver its result/error.

This pattern avoids busy-spins, keeps batches full under bursts, and flushes quickly under low load.

---

## Development

### Run tests

```bash
 TEI_URL=http://localhost:8080 cargo test
```

---

## Troubleshooting

* **503 / “overloaded / unavailable”**
  Increase `QUEUE_CAP` or reduce `-c` in the bench. If TEI is slow, reduce `BATCH_CONCURRENCY` to avoid upstream
  overload.

* **High latency tails**
  Lower `MAX_WAIT_TIME_MS`. If throughput drops too much, increase `MAX_BATCH_SIZE` and/or `BATCH_CONCURRENCY`
  cautiously.

* **TEI not reachable**
  Check `TEI_URL`, network, container names (`tei`), and that TEI is listening on port `80` inside the container (
  `8080:80` mapping).

---
