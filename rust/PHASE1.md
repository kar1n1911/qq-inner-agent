# Phase 1 configuration layer

`src/config.rs` implements the complete defaults snapshot, deep merge, validation,
normalization, environment precedence, lexical data path resolution and readiness.
`src/settings.rs` implements Node-compatible UTF-8 revision hashing, optional JSON
reads and 0600 temporary-file replacement. No dependency or lockfile changes.

Run from `rust/`:

```sh
cargo build --release
cargo clippy -- -D warnings
cargo test
cargo run -- --root /path/to/repository config
cargo run -- --root /path/to/repository selftest
```

Tests create disposable directories under `rust/target/`. Node parity tests run
normally when `node` is available and return with a SKIP message otherwise. They
import the repository's actual JS modules and compare complete loaded values,
readiness, defaults, validation failures and revisions. No HTTP/WS connections or
real data directories are opened. SQLite selftest stays in memory.

## API and compatibility boundaries

- `validate(&Value)` cannot mutate its argument. `normalize(&Value)` performs the
  two exact persona migrations and QQ ID string conversion after validation;
  `load_config` always uses it. `Loaded.raw` is the complete normalized JS-shaped
  object including unknown keys and derived credentials/dataDir.
- `Config` and all nested settings have typed fields documented with their JSON
  key. Numeric fields are `f64` because many JS ranges deliberately accept
  fractions. For text-like fields JS leaves untyped, `RuntimeText` caches
  `String(value)` and original JS truthiness; it does not require JSON lookup in
  the hot path. The unchanged value remains available in `Loaded.raw`.
- String lengths use UTF-16 units, trim uses the ECMAScript whitespace set, and
  digit/time matching uses ASCII, matching the JS expressions. `selfId` is not
  rewritten in raw config; only the three ID lists are rewritten by JS.
- URL parsing reuses ureq's existing WHATWG parser without making requests.
  Timezones use the existing chrono-tz database, case-insensitive names and Intl
  fixed-offset syntax. Node ICU and chrono-tz database/version differences can
  affect uncommon aliases or newly introduced zones; invalid-zone error wording
  is deliberately generic rather than embedding implementation-specific text.
- serde_json cannot represent lone UTF-16 surrogate strings that JSON.parse can
  accept. It also preserves large integer literals beyond JS's exact integer
  range, and rejects overflowing numeric literals that JS parses as Infinity.
  These pathological JSON inputs are not claimed byte-for-byte equivalent.
- `merge` reproduces JSON-visible array index updates but cannot retain JS array
  non-index properties (JSON serialization drops them anyway). Its `Value`
  return signature cannot express JS assignment exceptions when the base is a
  primitive; supported configuration merging starts from the defaults object.
- Atomic JSON output is pretty-printed with a trailing newline and mode 0600 on
  Unix. Object key order/number formatting can differ from JSON.stringify;
  revision always hashes the actual UTF-8-decoded file contents on both sides.
  Like JS, the fixed `.tmp` name assumes externally serialized writers and does
  not promise fsync durability. Unix permissions are the deployment contract.
