// Copyright 2023 Greptime Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::env;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use common_runtime::error::{Error, Result};
use common_runtime::{BoxedTaskFunction, RepeatedTask, Runtime, TaskFunction};
use common_telemetry::debug;
use once_cell::sync::Lazy;
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};

pub const TELEMETRY_URL: &str = "https://api-preview.greptime.cloud/db/otel/statistics";

// Getting the right path when running on windows
static TELEMETRY_UUID_FILE_NAME: Lazy<PathBuf> = Lazy::new(|| {
    let mut path = PathBuf::new();
    path.push(env::temp_dir());
    path.push(".greptimedb-telemetry-uuid");
    path
});

pub static TELEMETRY_INTERVAL: Duration = Duration::from_secs(60 * 30);

const GREPTIMEDB_TELEMETRY_CLIENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const GREPTIMEDB_TELEMETRY_CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

pub enum GreptimeDBTelemetryTask {
    Enable(RepeatedTask<Error>),
    Disable,
}

impl GreptimeDBTelemetryTask {
    pub fn enable(interval: Duration, task_fn: BoxedTaskFunction<Error>) -> Self {
        GreptimeDBTelemetryTask::Enable(RepeatedTask::new(interval, task_fn))
    }

    pub fn disable() -> Self {
        GreptimeDBTelemetryTask::Disable
    }

    pub fn start(&self, runtime: Runtime) -> Result<()> {
        match self {
            GreptimeDBTelemetryTask::Enable(task) => task.start(runtime),
            GreptimeDBTelemetryTask::Disable => Ok(()),
        }
    }

    pub async fn stop(&self) -> Result<()> {
        match self {
            GreptimeDBTelemetryTask::Enable(task) => task.stop().await,
            GreptimeDBTelemetryTask::Disable => Ok(()),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct StatisticData {
    pub os: String,
    pub version: String,
    pub arch: String,
    pub mode: Mode,
    pub git_commit: String,
    pub nodes: Option<i32>,
    pub uuid: String,
}

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Distributed,
    Standalone,
}

#[async_trait::async_trait]
pub trait Collector {
    fn get_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    fn get_git_hash(&self) -> String {
        env!("GIT_COMMIT").to_string()
    }

    fn get_os(&self) -> String {
        env::consts::OS.to_string()
    }

    fn get_arch(&self) -> String {
        env::consts::ARCH.to_string()
    }

    fn get_mode(&self) -> Mode;

    fn get_retry(&self) -> i32;

    fn inc_retry(&mut self);

    fn set_uuid_cache(&mut self, uuid: String);

    fn get_uuid_cache(&self) -> Option<String>;

    async fn get_nodes(&self) -> Option<i32>;

    fn get_uuid(&mut self) -> Option<String> {
        match self.get_uuid_cache() {
            Some(uuid) => Some(uuid),
            None => {
                if self.get_retry() > 3 {
                    return None;
                }
                match default_get_uuid() {
                    Some(uuid) => {
                        self.set_uuid_cache(uuid.clone());
                        Some(uuid)
                    }
                    None => {
                        self.inc_retry();
                        None
                    }
                }
            }
        }
    }
}

pub fn default_get_uuid() -> Option<String> {
    let path = (*TELEMETRY_UUID_FILE_NAME).as_path();
    match std::fs::read(path) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).to_string()),
        Err(e) => {
            if e.kind() == ErrorKind::NotFound {
                let uuid = uuid::Uuid::new_v4().to_string();
                let _ = std::fs::write(path, uuid.as_bytes());
                Some(uuid)
            } else {
                None
            }
        }
    }
}

/// Report version info to GreptimeDB.
/// We do not collect any identity-sensitive information.
/// This task is scheduled to run every 30 minutes.
/// The task will be disabled default. It can be enabled by setting the build feature `greptimedb-telemetry`
/// Collector is used to collect the version info. It can be implemented by different components.
/// client is used to send the HTTP request to GreptimeDB.
/// telemetry_url is the GreptimeDB url.
pub struct GreptimeDBTelemetry {
    statistics: Box<dyn Collector + Send + Sync>,
    client: Option<Client>,
    telemetry_url: &'static str,
}

#[async_trait::async_trait]
impl TaskFunction<Error> for GreptimeDBTelemetry {
    fn name(&self) -> &str {
        "Greptimedb-telemetry-task"
    }

    async fn call(&mut self) -> Result<()> {
        self.report_telemetry_info().await;
        Ok(())
    }
}

impl GreptimeDBTelemetry {
    pub fn new(statistics: Box<dyn Collector + Send + Sync>) -> Self {
        let client = Client::builder()
            .connect_timeout(GREPTIMEDB_TELEMETRY_CLIENT_CONNECT_TIMEOUT)
            .timeout(GREPTIMEDB_TELEMETRY_CLIENT_TIMEOUT)
            .build();
        Self {
            statistics,
            client: client.ok(),
            telemetry_url: TELEMETRY_URL,
        }
    }

    pub async fn report_telemetry_info(&mut self) -> Option<Response> {
        match self.statistics.get_uuid() {
            Some(uuid) => {
                let data = StatisticData {
                    os: self.statistics.get_os(),
                    version: self.statistics.get_version(),
                    git_commit: self.statistics.get_git_hash(),
                    arch: self.statistics.get_arch(),
                    mode: self.statistics.get_mode(),
                    nodes: self.statistics.get_nodes().await,
                    uuid,
                };

                if let Some(client) = self.client.as_ref() {
                    debug!("report version: {:?}", data);
                    let result = client.post(self.telemetry_url).json(&data).send().await;
                    debug!("report version result: {:?}", result);
                    result.ok()
                } else {
                    None
                }
            }
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::env;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use common_test_util::ports;
    use hyper::service::{make_service_fn, service_fn};
    use hyper::Server;
    use reqwest::Client;
    use tokio::spawn;

    use crate::{Collector, GreptimeDBTelemetry, Mode, StatisticData};

    static COUNT: AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    async fn echo(req: hyper::Request<hyper::Body>) -> hyper::Result<hyper::Response<hyper::Body>> {
        let path = req.uri().path();
        if path == "/req-cnt" {
            let body = hyper::Body::from(format!(
                "{}",
                COUNT.load(std::sync::atomic::Ordering::SeqCst)
            ));
            Ok(hyper::Response::new(body))
        } else {
            COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(hyper::Response::new(req.into_body()))
        }
    }

    #[tokio::test]
    async fn test_gretimedb_telemetry() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let port: u16 = ports::get_port() as u16;
        spawn(async move {
            let make_svc = make_service_fn(|_conn| {
                // This is the `Service` that will handle the connection.
                // `service_fn` is a helper to convert a function that
                // returns a Response into a `Service`.
                async { Ok::<_, Infallible>(service_fn(echo)) }
            });
            let addr = ([127, 0, 0, 1], port).into();

            let server = Server::bind(&addr).serve(make_svc);
            let graceful = server.with_graceful_shutdown(async {
                rx.await.ok();
            });
            let _ = graceful.await;
            Ok::<_, Infallible>(())
        });
        struct TestStatistic;

        struct FailedStatistic;

        #[async_trait::async_trait]
        impl Collector for TestStatistic {
            fn get_mode(&self) -> Mode {
                Mode::Standalone
            }

            async fn get_nodes(&self) -> Option<i32> {
                Some(1)
            }

            fn get_retry(&self) -> i32 {
                unimplemented!()
            }

            fn inc_retry(&mut self) {
                unimplemented!()
            }

            fn set_uuid_cache(&mut self, _: String) {
                unimplemented!()
            }

            fn get_uuid_cache(&self) -> Option<String> {
                unimplemented!()
            }

            fn get_uuid(&mut self) -> Option<String> {
                Some("test".to_string())
            }
        }

        #[async_trait::async_trait]
        impl Collector for FailedStatistic {
            fn get_mode(&self) -> Mode {
                Mode::Standalone
            }

            async fn get_nodes(&self) -> Option<i32> {
                None
            }

            fn get_retry(&self) -> i32 {
                unimplemented!()
            }

            fn inc_retry(&mut self) {
                unimplemented!()
            }

            fn set_uuid_cache(&mut self, _: String) {
                unimplemented!()
            }

            fn get_uuid_cache(&self) -> Option<String> {
                unimplemented!()
            }

            fn get_uuid(&mut self) -> Option<String> {
                None
            }
        }

        let test_statistic = Box::new(TestStatistic);
        let mut test_report = GreptimeDBTelemetry::new(test_statistic);
        let url = Box::leak(format!("{}:{}", "http://localhost", port).into_boxed_str());
        test_report.telemetry_url = url;
        let response = test_report.report_telemetry_info().await.unwrap();

        let body = response.json::<StatisticData>().await.unwrap();
        assert_eq!(env::consts::ARCH, body.arch);
        assert_eq!(env::consts::OS, body.os);
        assert_eq!(env!("CARGO_PKG_VERSION"), body.version);
        assert_eq!(env!("GIT_COMMIT"), body.git_commit);
        assert_eq!(Mode::Standalone, body.mode);
        assert_eq!(1, body.nodes.unwrap());

        let failed_statistic = Box::new(FailedStatistic);
        let mut failed_report = GreptimeDBTelemetry::new(failed_statistic);
        failed_report.telemetry_url = url;
        let response = failed_report.report_telemetry_info().await;
        assert!(response.is_none());

        let client = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();

        let cnt_url = format!("{}/req-cnt", url);
        let response = client.get(cnt_url).send().await.unwrap();
        let body = response.text().await.unwrap();
        assert_eq!("1", body);
        tx.send(()).unwrap();
    }
}