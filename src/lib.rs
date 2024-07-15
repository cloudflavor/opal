// Copyright 2024 Cloudflavor GmbH

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at

// http://www.apache.org/licenses/LICENSE-2.0

// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use structopt::StructOpt;

pub mod server;
pub mod nubium_opal_v1 {
    tonic::include_proto!("nubium.opal.v1");
}

#[derive(StructOpt, Debug)]
pub struct Opt {
    #[structopt(short, long, default_value = "127.0.0.1")]
    pub address: String,
    #[structopt(short, long, default_value = "9999")]
    pub port: u16,
    #[structopt(
        short,
        long,
        default_value = "info",
        possible_values = &["trace", "debug","info","warn","error"]
    )]
    pub log_level: tracing::Level,
}
