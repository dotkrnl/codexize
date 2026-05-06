use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn run_returns_ok_when_program_exits_zero() {
    // `true` exits 0 immediately; the success branch returns Ok(()).
    let result = run(WarmupSpec {
        program: "true",
        args: &[],
        script: "",
        env: &[],
        settle_timeout: Duration::from_secs(2),
    });
    assert!(
        result.is_ok(),
        "warmup with `true` should succeed: {:?}",
        result.err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_returns_err_when_program_exits_immediately_nonzero() {
    // `false` exits 1 within the 300ms grace window, hitting the
    // "warm-up exited immediately" bail.
    let result = run(WarmupSpec {
        program: "false",
        args: &[],
        script: "",
        env: &[],
        settle_timeout: Duration::from_secs(2),
    });
    let err = result.expect_err("warmup with `false` must error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("warm-up exited immediately"),
        "expected immediate-exit context: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_returns_ok_after_settle_timeout_kills_child() {
    // `sleep 5` outruns the 200ms settle timeout; the timeout branch
    // SIGKILLs the child and returns Ok(()).
    let result = run(WarmupSpec {
        program: "sleep",
        args: &["5"],
        script: "",
        env: &[],
        settle_timeout: Duration::from_millis(200),
    });
    assert!(
        result.is_ok(),
        "warmup must return Ok after killing on settle timeout: {:?}",
        result.err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_returns_err_when_spawn_fails() {
    let result = run(WarmupSpec {
        program: "/this/program/definitely/does/not/exist-xyz",
        args: &[],
        script: "",
        env: &[],
        settle_timeout: Duration::from_secs(1),
    });
    let err = result.expect_err("missing binary should fail spawn");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("failed to start") || msg.contains("warm-up"),
        "expected spawn-failure context: {msg}"
    );
}
