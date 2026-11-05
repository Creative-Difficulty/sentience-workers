# Fuzz targets for the LLM workers

Requires nightly and `cargo-fuzz`. Building compiles the worker crates, so
`DATABASE_URL` must point at a database with the unidb schema (sqlx macros)
unless an offline `.sqlx` cache is present.

| target         | input                          | checks |
|----------------|--------------------------------|--------|
| `fe_parse`     | raw LLM response bytes         | fact-extractor parse + sanitize never panic; sanitized records satisfy the DB invariants |
| `fe_sanitize`  | structured record (any floats, dates, strings) | sanitize never panics; output satisfies the DB invariants |
| `ts_parse`     | raw LLM response bytes         | topic-sorter parse + plan never panic; plan only uses batch ids, once each, under canonical names |
| `ts_normalize` | arbitrary string               | topic normalisation is canonical and idempotent |

```sh
cd fuzz
cargo +nightly fuzz run -s none fe_parse corpus/fe_parse seeds/fe_parse -- -max_total_time=600
```

Crashes found while fuzzing are turned into regression tests in the crates'
unit tests.
