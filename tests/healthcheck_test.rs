use httpmock::{Method::GET, MockServer};
use rust_healthcheck::{Config, run_healthchecks};

fn make_config(urls: Vec<String>) -> Config {
    Config {
        endpoints_to_check: urls,
        request_timeout_ms: 1500,
        concurrency: 4,
        retries: 0,
        base_backoff_ms: 50,
        max_backoff_ms: 200,
        user_agent: "rust-healthcheck/test".to_string(),
        log_level: Some("warn".to_string()),
        metrics_log_interval_sec: None,
        watch_interval_sec: None,
        cb_failures_threshold: 3,
        cb_cooldown_sec: 60,
        json_logging: false,
        summary_json: false,
        danger_accept_invalid_certs: false,
        ca_bundle_path: None,
        endpoints: None,
    }
}

#[tokio::test]
async fn it_marks_success_as_up() {
    let server = MockServer::start_async().await;
    let m1 = server
        .mock_async(|when, then| {
            when.method(GET).path("/ok");
            then.status(200).body("ok");
        })
        .await;

    let cfg = make_config(vec![format!("{}/ok", server.base_url())]);
    let summary = run_healthchecks(&cfg)
        .await
        .expect("run_healthchecks failed");
    m1.assert();
    assert_eq!(summary.total, 1);
    assert_eq!(summary.up, 1);
    assert_eq!(summary.down, 0);
}

#[tokio::test]
async fn it_marks_500_as_down() {
    let server = MockServer::start_async().await;
    let m1 = server
        .mock_async(|when, then| {
            when.method(GET).path("/err");
            then.status(500);
        })
        .await;

    let cfg = make_config(vec![format!("{}/err", server.base_url())]);
    let summary = run_healthchecks(&cfg)
        .await
        .expect("run_healthchecks failed");
    m1.assert();
    assert_eq!(summary.total, 1);
    assert_eq!(summary.up, 0);
    assert_eq!(summary.down, 1);
}

#[tokio::test]
async fn it_times_out() {
    let server = MockServer::start_async().await;
    let _m1 = server
        .mock_async(|when, then| {
            when.method(GET).path("/slow");
            then.status(200)
                .delay(std::time::Duration::from_millis(2_000));
        })
        .await;

    let cfg = make_config(vec![format!("{}/slow", server.base_url())]);
    let summary = run_healthchecks(&cfg)
        .await
        .expect("run_healthchecks failed");
    assert_eq!(summary.total, 1);
    assert_eq!(summary.up, 0);
    assert_eq!(summary.down, 1);
}
