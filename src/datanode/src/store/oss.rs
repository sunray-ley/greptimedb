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

use common_telemetry::logging::info;
use object_store::services::Oss as OSSBuilder;
use object_store::{util, ObjectStore};
use secrecy::ExposeSecret;
use snafu::prelude::*;

use crate::datanode::OssConfig;
use crate::error::{self, Result};

pub(crate) async fn new_oss_object_store(oss_config: &OssConfig) -> Result<ObjectStore> {
    let root = util::normalize_dir(&oss_config.root);
    info!(
        "The oss storage bucket is: {}, root is: {}",
        oss_config.bucket, &root
    );

    let mut builder = OSSBuilder::default();
    builder
        .root(&root)
        .bucket(&oss_config.bucket)
        .endpoint(&oss_config.endpoint)
        .access_key_id(oss_config.access_key_id.expose_secret())
        .access_key_secret(oss_config.access_key_secret.expose_secret());

    Ok(ObjectStore::new(builder)
        .context(error::InitBackendSnafu)?
        .finish())
}