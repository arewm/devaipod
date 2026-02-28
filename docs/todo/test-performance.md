# Integration Test Performance

## Current State (~243s for 65 tests)

- **Container build** with `--no-cache` every run: 60–180s
- **RUST_TEST_THREADS=1** (all serial): prevents parallelism even for safe tests
- **~29 per-test pod creations** at 5–15s each: ~150–200s
- **Fixed `sleep(2)` calls**: 12+ instances in `container.rs`, 4 in `ssh.rs` (~35s total)
- **WebFixture startup**: up to 90s (token wait + ready check)

## Done

- [x] Added containerized integration test runner (`just test-integration`)
- [x] Made `RUST_TEST_THREADS` overridable from environment

## Future Improvements

- [ ] Replace `sleep(Duration::from_secs(2))` with readiness polling (`container.rs` has 12 instances, `ssh.rs` has 4). Each could save 1–2s if the container starts in <1s. Estimated savings: 20–30s
- [ ] Enable parallel execution for web tests (Tier 1 tests documented as parallel-safe in `webui.rs`). Could reduce web test time by 50–70%
- [ ] Share pods across mutating tests where possible (`container.rs` creates ~21 pods). Grouping tests that only need "a running pod" could save 100–150s
- [ ] Reduce curl timeouts from 30s to 10s where appropriate (failing requests waste time)
- [ ] Consider splitting into quick vs full test suites for CI vs local
