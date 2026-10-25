use super::prepare_screen_line;

#[tokio::test]
async fn screen_preparation_runs_outside_the_runtime_worker() {
    let worker = std::thread::current().id();
    let line = prepare_screen_line(move || {
        assert_ne!(
            std::thread::current().id(),
            worker,
            "screen preparation must leave the async worker available for image output"
        );
        Ok("complete screen".to_string())
    })
    .await
    .expect("prepare screen");
    assert_eq!(line, "complete screen");
}

#[tokio::test]
async fn screen_preparation_preserves_the_error() {
    let error = prepare_screen_line(|| Err("screen cell is invalid".to_string()))
        .await
        .expect_err("reject invalid screen");
    assert_eq!(error, "screen cell is invalid");
}

#[tokio::test]
async fn screen_preparation_reports_a_failed_task() {
    let error = prepare_screen_line(|| panic!("fixture screen preparation panic"))
        .await
        .expect_err("report failed preparation task");
    assert!(error.starts_with("screen preparation task failed:"), "{error}");
    assert!(error.contains("fixture screen preparation panic"), "{error}");
}
